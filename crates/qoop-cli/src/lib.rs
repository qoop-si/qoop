//! `qoop`: attach to a terminal, starting the daemon if there is not one.
//!
//! The promise this milestone exists to keep is the small one:
//!
//! ```text
//! cd my-project
//! qoop
//! ```
//!
//! reaches a real shell, with no account, no wizard and no manifest, and detaching leaves
//! the shell running.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

mod arecord;
mod ask;
mod avatar;
mod board;
mod board_page;
mod config;
mod doctor;
mod hooks;
mod icons;
mod keep;
// Only the notch, a macOS feature, puts a launchd service back on a replaced binary.
#[cfg(target_os = "macos")]
mod launchd;
mod messages;
mod mirror;
mod notch;
mod onboard;
mod page;
mod places;
mod qass;
mod qass_brief;
mod qass_page;
mod remote;
pub mod report;
mod settings;
mod show;
mod skills;
mod sound;
mod supervise;
mod task;
mod task_verify;
mod uninstall;
mod update;
mod worktree;

use qoop_runtime::client::{self, RenderStream, TerminalGuard};
use qoop_runtime::daemon::ClientWire;
use qoop_runtime::{Daemon, handoff, lock, paths};

fn shell() -> PathBuf {
    // A login shell is what the user expects, but a shell chosen by an environment variable
    // is also the shell an attacker controls if they control the environment. It is read
    // here because the daemon runs as the user and the user's own SHELL is exactly the
    // right answer for a terminal multiplexer; ADR-015's rules apply to a Setup artifact,
    // which is a different and untrusted input.
    std::env::var("SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/bin/sh"))
}

/// What runs `qoop pair`, `qoop devices` and `qoop bridge`: the phone app's part, in the
/// release build of qoop and not in this repository.
pub type Phone = fn(&[String]) -> std::io::Result<()>;

/// `qoop`, with the phone app's part when the binary carries it.
pub fn main_with(phone: Option<Phone>) {
    // Called as `arecord` from a tab: the shim, before anything else touches the home, as
    // Claude Code checks it on every start of its voice.
    if arecord::called() {
        std::process::exit(arecord::run());
    }
    // Before anything reads a path: what a machine that ran qoro keeps under that name
    // moves to qoop, with the old name left as a link (see `legacy`).
    qoop_runtime::legacy::move_home();
    qoop_runtime::legacy::settle_binary_name();
    // One sentence and a failing exit, never the `Error: Custom { kind: .. }` dump that
    // returning the error from `main` prints: every refusal below is worded for a person.
    // Exit 2 when the command was called wrong, 1 when it failed.
    if let Err(e) = run(phone) {
        eprintln!("qoop: {}", worded(&e));
        std::process::exit(if is_usage(&e) {
            2
        } else if e
            .get_ref()
            .is_some_and(|inner| inner.is::<ask::Unavailable>())
        {
            // The inbox cannot take a card (an older daemon, or none): `escalate.sh` says so
            // and still rings.
            3
        } else if e.get_ref().is_some_and(|inner| inner.is::<ask::NotFree>()) {
            // The card number is not free: `escalate.sh new` takes the next.
            4
        } else {
            1
        });
    }
}

/// A refusal as a person reads it. A running daemon older than this binary answers a method
/// it does not know with "`qoop stop`, then `qoop`": advice that ends every tab, and that an
/// agent reading this output would follow. Every command's error passes here.
fn worded(e: &std::io::Error) -> String {
    let said = e.to_string();
    if said.contains("this daemon is older than the client") {
        return "the running qoop daemon is older than this command; it learns it the next \
                time it restarts on its own"
            .to_string();
    }
    said
}

/// A command called wrong, as opposed to one that failed: exits 2.
#[derive(Debug)]
struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

pub(crate) fn usage(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(Usage(message.into()))
}

fn is_usage(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Usage>())
}

fn run(phone: Option<Phone>) -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    // `--session` is taken out of the arguments wherever it was written, so every command's
    // own parsing sees what it expects: one daemon now, and only `default` names it.
    if let Err(e) = take_session(&mut args) {
        eprintln!("qoop: {e}");
        std::process::exit(2);
    }
    match args.get(1).map(String::as_str) {
        Some("daemon") => run_daemon(&args),
        // What a daemon about to move onto this binary asks of it: the handover layout it
        // takes over from. A build that has no such answer fails the question, and is
        // moved onto the old way.
        Some("--handoff-formats") => {
            println!("{}", handoff::FORMAT);
            Ok(())
        }
        Some("--hello") => {
            // What a remote bootstrap asks for. JSON on one line, because it is read
            // between markers out of whatever a login shell prints around it, and because
            // the thing that decides compatibility is the codec generation and not the
            // version string (ADR-010).
            println!(
                "{}",
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "codec_generation": qoop_proto::CODEC_GENERATION,
                    "platform": std::env::consts::OS,
                })
            );
            Ok(())
        }
        Some("attach") => run_attach(),
        Some("relay") => run_relay(),
        // What `qoop <remote>` asks of the machine before its interface opens: the space
        // that slept last back, or a first tab, as a bare `qoop` does here.
        Some("ensure-awake") => supervise::ensure_awake(),
        Some("remote") => remote::run(&args),
        Some("--remote") => run_remote(&args),
        Some("--space") => run_space(&args),
        Some("send") => run_send(&args),
        Some("read") => run_read(&args),
        Some("prompt") => run_prompt(&args),
        Some("history") => run_history(&args),
        Some("context") => run_context(&args),
        Some("messages") => messages::run(&args),
        Some("agents") => run_agents(&args),
        Some("focus") => run_focus(&args),
        Some("notch") => notch::run(&args),
        Some("wait") => supervise::run_wait(&args),
        Some("new") => supervise::run_new(&args),
        Some("sleep") => supervise::run_sleep(&args),
        Some("wake") => supervise::run_wake(&args),
        Some("close") => supervise::run_close(&args),
        Some("worktree") => worktree::run(&args),
        Some("space") => supervise::run_space(&args),
        Some("setup") => run_setup(&args),
        Some("hook-report") => run_hook_report(&args),
        Some("task") => task::run(&args),
        Some("ask") => ask::run(&args),
        Some("show") => show::run(&args),
        Some("doctor") => doctor::run(&args),
        Some("reload-manifests") => run_reload_manifests(),
        Some("hooks") => hooks::run(&args),
        Some("avatar") => avatar::run(&args),
        Some("config") => config::run(&args),
        Some("sound") => sound::run(&args),
        Some("settings") => settings::run(&args),
        Some("onboard") => onboard::run(&args),
        Some("skills") => skills::run(&args),
        Some("icons") => icons::run(&args),
        Some("paths") => places::run(&args[2..]),
        Some("qass") => qass::run_qass(&args),
        Some("report")
            if matches!(
                args.get(2).map(String::as_str),
                Some("send" | "delete" | "inbox")
            ) =>
        {
            phone_command(phone, &args)
        }
        Some("report") => report::run(&args),
        Some("update") => update::run(&args),
        Some("uninstall") => uninstall::run(&args, phone),
        Some("restart") => update::run_restart(&args),
        Some("pair" | "devices" | "bridge" | "login" | "logout") => phone_command(phone, &args),
        Some("ls") => run_list_spaces(&args),
        Some("stop") => run_stop(&args),
        Some("--version" | "-V") => {
            println!("{}", qoop_runtime::legacy::version_line());
            Ok(())
        }
        Some("--help" | "-h") => {
            print!(
                "qoop, the portable runtime for human and coding-agent workspaces\n\
                 \n\
                 usage:\n\
                 \x20 qoop            attach to a terminal, starting the daemon if needed\n\
                 \x20 qoop attach     attach to one terminal, with no interface around it\n\
                 \x20 qoop daemon     run the daemon in the foreground\n\
                 \x20 qoop reset      restore a terminal left in raw mode by a killed client\n\
                 \x20 qoop relay      pipe stdin and stdout to this computer's daemon\n\
                 \x20 qoop ensure-awake  wake the space that slept last, or open a first tab\n\
                 \x20 qoop --remote <name|ssh-target>  the interface for another computer's qoop\n\
                 \x20 qoop --space <id|name>  the interface with that one space in it, made\n\
                 \x20                 when the name is new; `qoop` still shows them all\n\
                 \x20 qoop remote add <name> <ssh-target>  show that\n\
                 \x20                 computer's spaces in `qoop`, beside this one's\n\
                 \x20 qoop remote rm <name>  stop showing them\n\
                 \x20 qoop remote ls  the remotes added\n\
                 \x20 qoop setup ...  capture, inspect, trust and run a Setup\n\
                   \x20 qoop hook-report --state <state> [--event user|tool|blocked|done] [--label ...] [--session-id S]  report this terminal's agent state\n\
                  \x20 qoop hook-report --session-id S  report only the agent's conversation id\n\
                  \x20 qoop hook-report --subagent-start ID|--subagent-end ID|--subagents ID,...  report its sub-agents\n\
                  \x20 qoop task new|goal|stage|plan|evidence|done|close|verify|ls|show|board|rules  the work this\n\
                  \x20                 terminal's agent is doing, kept by qoop; `qoop task` for more\n\
                  \x20 qoop task add|describe|steps|check|link|unlink|start|reopen  the board's items\n\
                  \x20 qoop ask add|close|ls  the owner's questions, as qass fills them;\n\
                  \x20                 `qoop ask` for more\n\
                  \x20 qoop show <file|url> [--note TEXT]  put a screenshot, recording,\n\
                  \x20                 report or public url on the owner's phone\n\
                  \x20 qoop doctor  whether the board, the questions and the agents agree;\n\
                  \x20                 read-only, `qoop doctor --help` for more\n\
                  \x20 qoop send <terminal> <text> [--enter]  type into another terminal\n\
                  \x20 qoop send <terminal> --key NAME...  press named keys: enter esc tab\n\
                  \x20                 shift-tab backspace space up down left right home end\n\
                  \x20                 pageup pagedown ctrl-a..ctrl-z\n\
                  \x20 qoop read <terminal> [--lines N] [--json] [--watch]  print another\n\
                  \x20                 terminal's screen; --back UP reads its history\n\
                  \x20 qoop prompt <target> [--wait] [--timeout SECS] [--] <text>  send text\n\
                  \x20                 plus Enter, unless blocked; --wait returns once the\n\
                  \x20                 agent took the turn and stopped\n\
                  \x20                 send, prompt and close take --generation G: refused\n\
                  \x20                 when the process is no longer the one that was read\n\
                  \x20 qoop wait [target...] [--until STATE,...] [--match TEXT] [--timeout SECS] [--json]\n\
                  \x20                 block until a terminal reaches a state, or TEXT is on\n\
                  \x20                 its live screen; exits 124 on timeout\n\
                  \x20 qoop sleep <space> | --tab N  end a space's tabs, or one tab, and keep them: nothing of it runs until it wakes\n\
                  \x20 qoop wake [space] | --tab N  bring a sleeping tab back, a space's, or the one that slept last\n\
                  \x20 qoop new [--name N] [--cwd DIR] [--space ID|NAME] [-- command...]  open a\n\
                  \x20                 terminal, print its number, type the command into it\n\
                  \x20 qoop worktree new <branch> [--base REF] [--path DIR] [--space ID|NAME] [--name NAME] [-- CMD...]\n\
                  \x20                 a git worktree beside the repo, with a terminal in it\n\
                  \x20 qoop worktree ls [--json]  every worktree and the terminals inside\n\
                  \x20 qoop worktree rm <branch|path> [--force]  remove one, keeping the\n\
                  \x20                 branch; refused while any process is inside\n\
                  \x20 qoop space rm <id|name>  remove a space with no tabs in it, asleep or awake\n\
                  \x20 qoop space goal <id|name> [text|-]  what you want from a space: print it, set it, an empty text clears it\n\
                  \x20 qoop close <terminal>  end a terminal and what it started\n\
                  \x20 qoop history <target> [--last N] [--role user|assistant] [--json]\n\
                  \x20                 print another agent's recent transcript window,\n\
                  \x20                 local files only; --json prints the full window\n\
                  \x20 qoop history --dir <repo> [--last N] [--json]\n\
                  \x20                 what you typed to your agents in a repo and its\n\
                  \x20                 worktrees (Claude Code, Codex, opencode; not Gemini)\n\
                  \x20 qoop messages <terminal> [--skip N] [--limit N] [--json]\n\
                  \x20                 the chat of an agent, parsed from its own session file:\n\
                  \x20                 one row per message, tool call or note, newest last\n\
                  \x20 qoop messages detail <terminal> <tool_id> [--json]\n\
                  \x20                 the input and output of one tool call of that chat\n\
                  \x20 qoop context <target> [--last N] [--role user|assistant] [--lines N] [--json]\n\
                  \x20                 understand another agent before writing to it:\n\
                  \x20                 who it is, whether it is writable, its live screen\n\
                  \x20                 and its recent transcript window\n\
                  \x20 qoop agents [--json]  list every terminal and what it is doing\n\
                  \x20 qoop agents --watch [--json]  the list again each time it changes\n\
                  \x20 qoop agents explain <target> [--json]  say why one terminal reads\n\
                  \x20                 the way it does: state, authority, matched rule\n\
                  \x20 qoop focus <terminal> [--json]  show that terminal in every\n\
                  \x20                 attached interface\n\
                 \x20 qoop reload-manifests  re-read agent-detection overrides\n\
                  \x20 qoop hooks ...  install an agent's own hook, so it says what it is\n\
                  \x20                 doing and which conversation it is having\n\
                  \x20 qoop skills ...  teach an agent what qoop is for, so it looks\n\
                  \x20                 before it writes\n\
                  \x20 qoop ls [--json]  list the spaces and say which are asleep\n\
                  \x20 qoop paths [name] [--json]  where qoop keeps each kind of file, and how big\n\
                  \x20 qoop uninstall [--yes]  take qoop off this computer: the steps of the Uninstall\n\
                  \x20                 docs page, listed and asked about first\n\
                  \x20 qoop avatar pick  choose a terminal pet from a batch\n\
                  \x20 qoop avatar show [--json]  print the picked pet\n\
                  \x20 qoop avatar options|set ...  the parts a pet may have, or set some\n\
                  \x20 qoop config [--json]  the settings in config.toml and where it is\n\
                  \x20 qoop config set <key> <value>  change one, keeping the rest of the file\n\
                  \x20 qoop settings   every setting on one page, with your pet\n\
                  \x20 qoop onboard    the first start: pet, sounds, look, keys, notifications, sleep, qass\n\
                  \x20 qoop qass [page]  open qass, the assistant over your agents, or its page\n\
                  \x20 qoop qass brief <n> [--json]  what qass must know before it writes to agent n\n\
                  \x20 qoop qass watch [on|off]  whether qass watches your agents, or wakes only when you write\n\
                  \x20 qoop report draft|list|show|rm|send|delete  qass's notes to qoop's maker\n\
                  \x20                 about its own way of working, checked and shown first\n\
                  \x20 qoop sound      hear the notification sounds and keep one each\n\
                  \x20 qoop notch install|uninstall|status  on a Mac, your agents in\n\
                  \x20                 the notch, started at login\n\
                  \x20 qoop icons install|uninstall|status  on a Mac, the font qoop's\n\
                  \x20                 icons are drawn from\n\
                 \x20 qoop stop       end the daemon and every tab; `qoop` brings them back\n\
                 \x20 qoop restart    move the daemon onto the qoop on disk now, every tab and agent still running\n\
                 \x20 qoop pair [--hub URL]  pair a phone: a QR code, then the code it shows\n\
                 \x20 qoop login [--hub URL]  link this computer to your account: it then shows\n\
                 \x20                 on qoop.si/app and on your phone\n\
                 \x20 qoop logout [--hub URL]  unlink this computer from its account\n\
                 \x20 qoop devices ls  the phones paired with this computer\n\
                 \x20 qoop devices rm <name>  unpair one; its open streams close at once\n\
                 \x20 qoop bridge     run the phones' end in the foreground (the service\n\
                 \x20                 that pairing installs runs exactly this)\n\
                 \x20 qoop bridge reload  put that service back on the installed binary\n\
                 \x20 qoop update [--check]  fetch the newest release, install it, and move every\n\
                 \x20                 computer onto it, keeping running tabs running where it can\n\
                 \x20 qoop --version\n\
                 \n\
                 detach with ctrl-q. the shell keeps running.\n\
                 a space with nothing awake costs nothing: `qoop sleep` it, and `qoop wake` brings it back.\n"
            );
            Ok(())
        }
        Some("reset") => {
            // SIGKILL cannot be caught, so a killed client can leave the outer terminal in
            // raw mode. This is the escape hatch the honest version of that guarantee needs.
            print!(
                "\x1b]72;t=A\x1b\\\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[?2031l\x1b[?25h\x1b[?1049l"
            );
            std::io::stdout().flush()?;
            Command::new("stty").arg("sane").status().ok();
            Ok(())
        }
        None => run_client(),
        // A typo is named, not answered with a daemon and the full-screen interface.
        Some(other) => Err(usage(format!(
            "unknown command {other}; usage: qoop [command], see qoop --help"
        ))),
    }
}

fn run_daemon(args: &[String]) -> std::io::Result<()> {
    // Where this binary is, read first and resolved through any link, before an update
    // can rename a new file over it: on Linux the running path then reads
    // `<path> (deleted)`. It is the file an update replaces and what the daemon becomes.
    let exe = std::env::current_exe().map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe));
    // Which file that is, for the daemon's look to stop once an update replaced it.
    let born = exe.as_deref().ok().and_then(keep::identity);
    // The dry run a daemon about to become this binary makes of it: can this read what it
    // wrote? Nothing is started and nothing is deleted.
    if let Some((file, nonce)) = handoff::check_requested(args) {
        handoff::check(&file, &nonce)?;
        // The state it would open next: a database from a newer build is found out here,
        // with every tab still in the daemon that is asking, and not after the exec.
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let db =
            qoop_runtime::legacy::db_path(&paths::state_dir(&dir).map_err(std::io::Error::other)?);
        if db.exists() {
            qoop_store::Store::check_openable(&db).map_err(std::io::Error::other)?;
        }
        return Ok(());
    }
    // The tabs the daemon before this one handed over, read before anything is closed:
    // their descriptors are the ones inherited that stay open.
    let mut orphans = Vec::new();
    let mut refused = None;
    let handed = handoff::requested(args).and_then(|(file, nonce)| {
        // Named before the read, which deletes the file: if it is refused, the shells it
        // names lose their terminals as the descriptors close, and are waited for.
        let named = handoff::children_in(&file);
        handoff::read(&file, &nonce)
            .map_err(|e| {
                refused = Some(e);
                orphans = named;
            })
            .ok()
    });
    qoop_runtime::daemon::close_inherited_fds(
        &handed
            .as_ref()
            .map(handoff::Handoff::fds)
            .unwrap_or_default(),
    );
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    log_to_file(&dir);
    if let Some(e) = refused {
        qoop_runtime::say!("qoopd: the handover was refused: {e}");
    }
    // Held across the whole sequence: stale-socket check, unlink, bind, listen, initialise.
    // Two daemons started at once therefore serialise here rather than both concluding the
    // socket is stale and unlinking each other's.
    // Or, after a move, the one the daemon before this held across it: nobody could take it
    // in between, so nobody started a second daemon.
    let lock = match handed.as_ref().and_then(|h| h.startup_lock) {
        Some(fd) if qoop_runtime::daemon::fd_is_open(fd) => {
            let _ = qoop_pty::sys::set_cloexec(fd);
            // SAFETY: the daemon before this one passed this descriptor on as the startup
            // lock, and nothing in this process owns it.
            unsafe { lock::StartupLock::from_fd(fd) }
        }
        _ => {
            let path = paths::lock_path(&dir);
            match lock::StartupLock::try_acquire(&path)? {
                Some(lock) => lock,
                None => {
                    // Said, because a daemon that waits here for long is a different
                    // thing from one that is not running, and nothing else tells them apart.
                    qoop_runtime::say!(
                        "qoopd: waiting for the startup lock, another daemon holds it"
                    );
                    lock::StartupLock::acquire(&path)?
                }
            }
        }
    };
    // Handover files nobody will read, from a daemon that died on the way.
    if let Some(dir) = paths::socket_path(&dir).parent() {
        handoff::sweep(dir);
    }
    let carried_tabs = handed.as_ref().map_or(0, |h| h.terminals.len());
    // Why a start failed, left where `ensure_daemon` looks: a daemon started on demand has
    // no terminal to say it on, and "the daemon did not start" alone sent people to the
    // socket when the cause was the data directory.
    let failed = dir.join("qoopd.failed");
    let _ = std::fs::remove_file(&failed);
    let state = match paths::state_dir(&dir) {
        Ok(state) => state,
        Err(e) => {
            let _ = std::fs::write(&failed, e.to_string());
            return Err(std::io::Error::other(e));
        }
    };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let daemon = match Daemon::bind_adopting(&dir, &state, &shell(), handed).await {
            Ok(d) => match exe {
                Ok(exe) => {
                    // Only a binary the release script built looks for newer ones: a
                    // build made by hand, a test's, a copy someone put on their PATH never
                    // forks a child on its own.
                    if update::release_built() {
                        tokio::spawn(update::auto_timer(exe.clone()));
                    }
                    if let Some(keeper) = keep::Keeper::for_this(&exe, born) {
                        tokio::spawn(keeper.run());
                    }
                    d.leaving_when_idle().restarting_into(exe)
                }
                Err(e) => {
                    qoop_runtime::say!("qoopd: where this binary is could not be read: {e}");
                    d.leaving_when_idle()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Another daemon won the race and is already serving. This is the expected
                // outcome for the loser, not a failure, unless this was a move: then the
                // tabs it carried lose their terminals as this process goes, and their rows
                // (written down before the move) bring them back.
                if carried_tabs > 0 {
                    // A failure and not a quiet exit: the tabs carried here are lost with
                    // this process, and a unit that restarts on failure should look again.
                    let why = format!(
                        "another daemon is serving this socket; what was carried here ({}) is left \
                         to it and restarts from the rows",
                        count(carried_tabs, "tab")
                    );
                    qoop_runtime::say!("qoopd: {why}");
                    return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, why));
                }
                qoop_runtime::say!("qoopd: another daemon is already serving this socket");
                return Ok(());
            }
            Err(e) => {
                let _ = std::fs::write(&failed, e.to_string());
                return Err(e);
            }
        };
        qoop_runtime::say::serving();
        qoop_runtime::say!("qoopd: listening on {}", daemon.socket().display());
        if !orphans.is_empty() {
            tokio::spawn(qoop_runtime::daemon::reap_children(orphans));
        }
        drop(lock);
        daemon.serve().await
    })
}

/// Where a daemon started on demand speaks: its starters give it `/dev/null` for stderr, so
/// what it said (a tab lost on a move, a refused handover, a panic) went nowhere. It writes to
/// `qoopd.log` in the logs directory instead, or in its session directory `session` when
/// `QOOP_RUNTIME_DIR` names one, so an isolated stack never writes into the person's log
/// (`qoop_runtime::say`).
fn log_to_file(session: &std::path::Path) {
    let explicit = qoop_runtime::legacy::var_os(paths::RUNTIME_DIR_ENV).is_some();
    let dir = if explicit {
        Some(session.to_path_buf())
    } else {
        paths::logs_dir()
    };
    if dir.is_some_and(|d| qoop_runtime::say::to_file(d.join("qoopd.log"))) {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        qoop_runtime::say!("[{at}] qoopd {} starting", env!("CARGO_PKG_VERSION"));
    }
}

/// Start the daemon if there is not one, then attach.
fn run_client() -> std::io::Result<()> {
    let focus = own_focus();
    let exe = own_path();
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);

    // The startup lock belongs to the daemon, not to the client.
    //
    // An earlier version had the client hold it while waiting for the daemon to come up,
    // which deadlocked: the daemon blocks acquiring the same lock, the client waits five
    // seconds for a socket that cannot appear, gives up, releases the lock, and then fails
    // to connect. It passed every unit test and failed the first time it was run in a real
    // terminal, with an unhelpful ENOENT.
    //
    // Two clients racing here both spawn a daemon, and that is fine: the daemon holds the
    // lock across its whole bind sequence, so the loser finds the socket already listening
    // and exits without touching it.
    ensure_daemon(&socket)?;
    supervise::ensure_awake()?;

    let rt = tokio::runtime::Runtime::new()?;
    // The interface, not the bare attach. `qoop attach` is still there for the one terminal
    // case and for anything driving this program from a script, because a full screen
    // interface is the wrong thing to hand a pipe.
    // Always through the mux, remotes or not: a machine connected from the `+` has to have
    // somewhere to go without restarting the interface. With none added it relays this
    // machine's renders as they come, undecoded.
    let (remotes, notice) = match qoop_remote::saved::load() {
        Ok(remotes) => (remotes, None),
        Err(e) => (Vec::new(), Some(format!("remotes not loaded: {e}"))),
    };
    let ran = rt.block_on(qoop_tui::mux::run(
        Some(&socket),
        remotes,
        notice,
        None,
        focus,
    ));
    restart_in_place(exe, &socket, ran)
}

/// This binary's path, resolved through any link before an update can rename a new file over
/// it: on Linux the running path then reads `<path> (deleted)`.
pub(crate) fn own_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// After an interface that ended because its daemon moved: this process becomes the qoop on
/// disk with the arguments it had, so the same terminal, space and tab are back at once (the
/// last place is kept on disk). The terminal was restored when the interface ended. Quitting
/// never gets here: only the daemon closing its side does (see [`moved`]).
fn restart_in_place(
    exe: Option<std::path::PathBuf>,
    socket: &std::path::Path,
    ran: std::io::Result<()>,
) -> std::io::Result<()> {
    if !moved(&ran, socket, CAME_BACK) {
        return ran;
    }
    let Some(exe) = exe else { return Ok(()) };
    use std::os::unix::process::CommandExt;
    let mut again = Command::new(&exe);
    again.args(std::env::args_os().skip(1));
    if let Some(terminal) = qoop_tui::run::closed_on() {
        again.env(qoop_tui::run::FOCUS_ENV, terminal.to_string());
    }
    // Keys typed while the window was replaced were meant for a box it no longer shows (a
    // `Y` for the close box): dropped, never typed into the pane the new window opens on.
    // SAFETY: tcflush on standard input only discards what is queued there.
    unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
    let failed = again.exec();
    Err(std::io::Error::other(format!(
        "the daemon moved but {} could not be started again: {failed}",
        exe.display()
    )))
}

/// The terminal this window showed before it became the qoop on disk ([`restart_in_place`]),
/// taken out of the environment so that nothing this window starts carries it.
fn own_focus() -> Option<u32> {
    let own = std::env::var_os(qoop_tui::run::FOCUS_ENV)?;
    // SAFETY: first thing in the interface, before the runtime or anything else starts a
    // thread.
    unsafe { std::env::remove_var(qoop_tui::run::FOCUS_ENV) };
    own.to_str()?.parse().ok().filter(|t| *t != 0)
}

/// How long a daemon that closed an interface with no click is looked for on its socket.
/// Past the moment an exec has the listener again, which is at once; under the few seconds
/// after which a person reads a window that hangs as qoop being stuck.
const CAME_BACK: Duration = Duration::from_secs(3);

/// Whether an interface that ended with `ran` ended because its daemon moved. After a click
/// on `restart for update`, the interface knows. With no click, a daemon answering on
/// `socket` within `within` is the one that moved all the same: `qoop restart` and `qoop
/// update` move it from another terminal (the qass skill restarts after every install), and
/// a daemon that let a stalled window go is still there. Nothing answering is the daemon
/// gone, and the interface's own error says so.
fn moved(ran: &std::io::Result<()>, socket: &std::path::Path, within: Duration) -> bool {
    match ran {
        Ok(()) => qoop_tui::run::restarting(),
        Err(e) => qoop_tui::run::DaemonClosed::ended(e) && answers(socket, within),
    }
}

/// Whether a daemon listens on `socket` within `within`, the way [`ensure_daemon`] asks.
fn answers(socket: &std::path::Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `qoop --space <id|name>`: the interface with one space in it and nothing else. The
/// space is made when the name is new, and woken, or given a tab, when there is nothing in
/// it to show. Remotes are left out: their spaces would be filtered away anyway.
fn run_space(args: &[String]) -> std::io::Result<()> {
    let focus = own_focus();
    let exe = own_path();
    let [_, _, wanted] = args else {
        return Err(usage("usage: qoop --space <id|name>"));
    };
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    ensure_daemon(&socket)?;
    let space = supervise::open_space(wanted)?;
    let rt = tokio::runtime::Runtime::new()?;
    let ran = rt.block_on(qoop_tui::mux::run(
        Some(&socket),
        Vec::new(),
        None,
        Some(space),
        focus,
    ));
    restart_in_place(exe, &socket, ran)
}

/// `qoop --remote <name|target>`: the interface for another machine's qoop alone.
///
/// A name added with `qoop remote add` brings its own target; anything else is an ssh
/// target. It goes through the same connection the interface keeps for added remotes, so a
/// dropped connection comes back on its own rather than ending the interface.
fn run_remote(args: &[String]) -> std::io::Result<()> {
    let Some(what) = args.get(2) else {
        eprintln!("usage: qoop --remote <name|ssh-target>");
        std::process::exit(2);
    };
    let added = qoop_remote::saved::load()?
        .into_iter()
        .find(|r| &r.name == what);
    let remote = match added {
        Some(remote) => remote,
        None if qoop_remote::saved::valid_target(what) => qoop_remote::Remote {
            name: what.clone(),
            target: what.clone(),
        },
        None => {
            eprintln!("qoop: {what:?} is neither an added remote nor an ssh target");
            std::process::exit(2);
        }
    };
    // In the foreground first, where ssh can ask whatever it needs to, and where a remote
    // without qoop says so before the screen is taken over.
    if let Err(e) = remote::check(&remote) {
        eprintln!("qoop: {e}");
        std::process::exit(1);
    }
    remote::wake(&remote);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(qoop_tui::mux::run(None, vec![remote], None, None, None))
}

/// Attach to one terminal, with no interface around it.
fn run_attach() -> std::io::Result<()> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    ensure_daemon(&socket)?;
    supervise::ensure_awake()?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(attach(&socket))
}

async fn attach(socket: &std::path::Path) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (stream, leftover) = client::connect(socket, "qoop-cli").await?;
    let guard = TerminalGuard::enter_raw()?;
    // A panic must not leave the outer terminal raw either.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        previous_hook(info);
    }));

    let (mut rd, mut wr) = stream.into_split();
    let mut seq = 0u32;
    let (tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    client::spawn_stdin_reader(tx);

    // Tell the daemon our size before anything else, so the shell starts at the right one.
    let (cols, rows) = client::outer_size();
    wr.write_all(&client::encode_command_seq(
        &ClientWire::Resize { cols, rows },
        &mut seq,
    )?)
    .await?;

    // Whatever arrived alongside the welcome, which can include the first snapshot.
    let mut inbuf = leftover;
    inbuf.reserve(256 * 1024);
    let mut read = [0u8; 64 * 1024];
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut out = std::io::stdout();
    // An interval, not a fresh sleep inside the select. A new sleep on every loop iteration
    // measures "two seconds in which nothing at all happened", so a client taking any other
    // arm regularly, a window drag delivering SIGWINCH every 50 ms for instance, never
    // acknowledges at all. ADR-013 asks for two seconds since the last acknowledgement.
    let mut ack_tick = tokio::time::interval(client::ACK_INTERVAL);
    ack_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut renders = RenderStream::new();
    // The window this client shows of the terminal's grid. `None` until the first snapshot,
    // because the grid's size belongs to its owner and is not known before then.
    let mut view: Option<client::Viewport> = None;

    // Draw whatever already arrived, before waiting for anything new. Carrying the leftover
    // and then not drawing it would leave the same blank screen the leftover exists to fix.
    if let Some(snapshot) = renders.take_latest(&mut inbuf)? {
        let (cols, rows) = client::outer_size();
        let next = client::viewport_for(&snapshot, view, cols, rows);
        view = Some(next);
        client::render_viewport(&snapshot, next, &mut out)?;
    }

    loop {
        tokio::select! {
            chunk = stdin_rx.recv() => {
                let Some(chunk) = chunk else { break };
                // Ctrl-Q detaches. The shell keeps running, which is the whole point.
                if chunk.contains(&0x11) {
                    break;
                }
                // Split like the interface does: one huge message trips the
                // reassembly bound and drops the paste whole.
                for piece in client::split_input(&chunk) {
                    wr.write_all(&client::encode_command_seq(
                        &ClientWire::Input {
                            bytes: piece.to_vec(),
                            // Attach typing is the person's own hands on
                            // this terminal, never a wait edge: no claim.
                            from_terminal: None,
                            from_token: None,
                        },
                        &mut seq,
                    )?)
                    .await?;
                }
            }
            n = rd.read(&mut read) => {
                let n = n?;
                if n == 0 {
                    break;
                }
                inbuf.extend_from_slice(&read[..n]);
                if let Some(snapshot) = renders.take_latest(&mut inbuf)? {
                    let (cols, rows) = client::outer_size();
                    let next = client::viewport_for(&snapshot, view, cols, rows);
                    view = Some(next);
                    client::render_viewport(&snapshot, next, &mut out)?;
                }
                if let Some(ack) = renders.due_ack(Instant::now()) {
                    wr.write_all(&ack).await?;
                }
            }
            _ = sigwinch.recv() => {
                // The window is recomputed on the next snapshot, so it is dropped here
                // rather than resized: a window remembered across a resize is a window that
                // no longer fits.
                view = None;
                let (cols, rows) = client::outer_size();
                wr.write_all(&client::encode_command_seq(&ClientWire::Resize { cols, rows }, &mut seq)?).await?;
            }
            _ = ack_tick.tick() => {
                // A client that never types still has to say it is keeping up. Without
                // this arm the acknowledgement would only ever ride on a keystroke, and a
                // silent client would look identical to a stalled one.
                if let Some(ack) = renders.due_ack(Instant::now()) {
                    wr.write_all(&ack).await?;
                }
            }
            _ = sigterm.recv() => break,
            _ = sighup.recv() => break,
        }
    }

    drop(guard);
    println!("\r\n[detached; the shell is still running]");
    Ok(())
}

/// `qoop setup`: capture, inspect, trust and run.
///
/// Trust is a separate verb on purpose. A Setup that arrives from another machine is
/// executable input (ADR-015), and the person who runs it has to have seen what it will do.
fn run_setup(args: &[String]) -> std::io::Result<()> {
    use qoop_setup::{ConfirmationLedger, level_of};

    // Under the runtime base, not a session's directory: trust is per machine (ADR-015),
    // and a Setup confirmed in one session was otherwise untrusted in every other one.
    // Resolved only by the verbs that read it, so a typo or the usage touches nothing.
    let ledger_path = || -> std::io::Result<std::path::PathBuf> {
        let base = paths::runtime_base().map_err(std::io::Error::other)?;
        Ok(base.join("confirmed-setups"))
    };

    match args.get(2).map(String::as_str) {
        Some("show") => {
            let path = args
                .get(3)
                .ok_or_else(|| usage("usage: qoop setup show <file>"))?;
            let setup = read_setup(path)?;
            let (level, gaps) = level_of(&setup);
            print!("{}", setup.confirmation_text());
            println!("  content hash {}", setup.content_hash());
            println!("  reproducible {level:?}");
            for gap in gaps {
                println!("  gap          {gap:?}");
            }
            Ok(())
        }
        Some("trust") => {
            let path = args
                .get(3)
                .ok_or_else(|| usage("usage: qoop setup trust <file>"))?;
            let setup = read_setup(path)?;
            // Shown before it is trusted, in full. The value of the prompt is that it is
            // rare enough to read, so there is no way to trust something without seeing it.
            // And the question is really asked: only a typed yes trusts it, and anything
            // else, an empty line or no input at all, leaves the ledger untouched.
            print!("{}\nRun this? [y/N] ", setup.confirmation_text());
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                return Err(std::io::Error::other(format!(
                    "not confirmed: {} stays untrusted",
                    setup.name
                )));
            }
            let mut ledger = ConfirmationLedger::open(&ledger_path()?);
            ledger
                .confirm(&setup.content_hash())
                .map_err(std::io::Error::other)?;
            println!("confirmed {} as {}", setup.name, setup.content_hash());
            Ok(())
        }
        Some("check") => {
            let path = args
                .get(3)
                .ok_or_else(|| usage("usage: qoop setup check <file>"))?;
            let setup = read_setup(path)?;
            let ledger = ConfirmationLedger::open(&ledger_path()?);
            match qoop_setup::ledger::admit(&setup, &ledger, true) {
                Ok(()) => {
                    println!("confirmed");
                    Ok(())
                }
                // Said as a sentence, like every other refusal, not as the type's dump.
                Err(qoop_setup::ledger::Refusal::NeedsConfirmation { prompt }) => Err(
                    std::io::Error::other(format!("not confirmed yet:\n{prompt}")),
                ),
                Err(qoop_setup::ledger::Refusal::NeedsConfirmationNoClient { name }) => {
                    Err(std::io::Error::other(format!(
                        "{name} is not confirmed, and nobody is here to ask"
                    )))
                }
            }
        }
        None | Some("--help" | "-h") => {
            print!(
                "usage:\n\
                 \x20 qoop setup show <file>    what it will run, its hash and its level\n\
                 \x20 qoop setup trust <file>   confirm it on this machine\n\
                 \x20 qoop setup check <file>   whether it may run here\n"
            );
            Ok(())
        }
        Some(other) => Err(usage(format!(
            "unknown command {other}; usage: qoop setup <show|trust|check> <file>"
        ))),
    }
}

fn read_setup(path: &str) -> std::io::Result<qoop_setup::Setup> {
    let text = std::fs::read_to_string(path)?;
    qoop_setup::Setup::from_toml(&text).map_err(|e| std::io::Error::other(e.to_string()))
}

/// `qoop hook-report`: what agent hook scripts call.
///
/// Reads the terminal and token the daemon put in the environment, posts one
/// `ReportAgentState` request, and waits for the answer. Quiet on success: a
/// hook that reports every turn must not spam the transcript. Failures go to
/// stderr with a non-zero exit, and hook scripts swallow them on purpose: the
/// screen reading keeps working with no hooks installed.
fn run_hook_report(args: &[String]) -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};

    let mut state: Option<String> = None;
    let mut terminal: Option<u32> = qoop_runtime::legacy::var("QOOP_TERMINAL")
        .ok()
        .and_then(|v| v.parse().ok());
    let mut token: Option<String> = qoop_runtime::legacy::var("QOOP_TOKEN").ok();
    let mut visible_blocker = false;
    let mut visible_idle = false;
    let mut visible_working = false;
    let mut id: Option<String> = None;
    let mut session: Option<String> = None;
    let mut from: Option<String> = None;
    let mut event: Option<String> = None;
    let mut label: Option<String> = None;
    let mut subagent: Option<qoop_runtime::control::SubagentChange> = None;
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--state" => {
                state = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            "--event" => {
                event = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            "--label" => {
                label = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            "--terminal" => {
                terminal = match rest.next() {
                    Some(v) => v.parse().ok(),
                    None => None,
                };
                if terminal.is_none() {
                    return Err(usage(
                        "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                    ));
                }
            }
            "--token" => {
                token = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            "--visible-blocker" => visible_blocker = true,
            "--visible-idle" => visible_idle = true,
            "--visible-working" => visible_working = true,
            "--id" => {
                id = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            // Which agent's hook this is: the daemon refuses a report from an agent that is
            // not the terminal's own, one the terminal's agent started.
            "--agent" => {
                from = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => return Err(usage("usage: qoop hook-report ... --agent <name>")),
                }
            }
            // The agent's own id for this conversation. Optional: most hooks do not know
            // one, and the ones that do are what makes a restart come back to this
            // conversation rather than to the newest one in the directory.
            "--session-id" => {
                session = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            // What the agent's own sub-agents did, by its id for each: counted beside the
            // agent, never a state. `--subagents` is its own list of what still runs, comma separated.
            flag @ ("--subagent-start" | "--subagent-end" | "--subagents") => {
                use qoop_runtime::control::SubagentChange;
                let Some(value) = rest.next() else {
                    return Err(usage(format!("usage: qoop hook-report {flag} <id>")));
                };
                subagent = Some(match flag {
                    "--subagent-start" => SubagentChange::Started(value.clone()),
                    "--subagent-end" => SubagentChange::Ended(value.clone()),
                    _ => SubagentChange::Running(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|id| !id.is_empty())
                            .map(String::from)
                            .collect(),
                    ),
                });
            }
            other => {
                return Err(usage(format!(
                    "unknown flag {other}; usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T] or qoop hook-report --subagent-start ID|--subagent-end ID|--subagents ID,..."
                )));
            }
        }
    }
    let Some(terminal) = terminal else {
        return Err(std::io::Error::other(
            "no terminal: pass --terminal or run inside a qoop terminal (QOOP_TERMINAL)",
        ));
    };
    let Some(token) = token else {
        return Err(std::io::Error::other(
            "no token: pass --token or run inside a qoop terminal (QOOP_TOKEN)",
        ));
    };
    // qass's own conversation, kept in its folder once the daemon took the report (a late
    // hook of a closed tab is refused): the daemon forgets a closed tab's id.
    let kept = session.clone();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Session identity without a state claim: a third-party agent that knows
    // its conversation but has no lifecycle event to map. State and session
    // travel separately because saying which conversation this is says
    // nothing about what the agent is doing.
    let request = match (state, subagent) {
        (state, Some(change)) => {
            if state.is_some() || event.is_some() || label.is_some() || session.is_some() {
                return Err(usage(
                    "usage: --subagent-start/--subagent-end/--subagents stand alone",
                ));
            }
            let id = id.unwrap_or_else(|| format!("hook-subagents-{terminal}-{nanos}"));
            ControlRequest::ReportSubagents {
                from: from.clone(),
                id,
                terminal,
                token,
                change,
            }
        }
        (None, None) => {
            if event.is_some() || label.is_some() {
                return Err(usage(
                    "usage: --event/--label need --state; or qoop hook-report --session-id S [--terminal N] [--token T]",
                ));
            }
            let Some(session) = session.filter(|s| !s.trim().is_empty()) else {
                return Err(usage(
                    "usage: qoop hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoop hook-report --session-id S [--terminal N] [--token T]",
                ));
            };
            let id = id.unwrap_or_else(|| format!("hook-session-{terminal}-{nanos}"));
            ControlRequest::ReportAgentSession {
                from: from.clone(),
                id,
                terminal,
                token,
                session,
            }
        }
        (Some(state), None) => {
            if qoop_runtime::control::parse_reported_state(&state).is_none() {
                return Err(std::io::Error::other(format!(
                    "unknown agent state {state:?}"
                )));
            }
            if let Some(ref kind) = event
                && !qoop_runtime::control::parse_focus_event(kind)
            {
                return Err(usage(format!(
                    "unknown focus event {kind:?}; usage: qoop hook-report --state <state> [--event user|tool|blocked|done] [--label ...]"
                )));
            }
            if label.is_some() && event.is_none() {
                return Err(usage("usage: --label needs --event user|tool|blocked|done"));
            }
            let id = id.unwrap_or_else(|| format!("hook-{terminal}-{nanos}"));
            ControlRequest::ReportAgentState {
                from: from.clone(),
                id,
                terminal,
                token,
                state,
                visible_blocker,
                visible_idle,
                visible_working,
                clock: nanos,
                session,
                event,
                label,
            }
        }
    };
    let id = request.request_id().to_string();

    // One thread, as `stop` uses: this runs on every tool call an agent makes, and a
    // runtime with a worker per core was that many threads spawned and joined for one
    // request. `enable_all` because the bound below is a timer and the socket is IO.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let attempt = || async {
            let (stream, mut inbuf) = client::connect(&socket, "qoop-hook").await?;
            let (mut rd, mut wr) = stream.into_split();
            let mut seq = 0u32;
            wr.write_all(&client::encode_control_seq(&request, &mut seq)?)
                .await?;
            let mut connection = client::Connection::new();
            let mut read = [0u8; 64 * 1024];
            loop {
                let n = rd.read(&mut read).await?;
                if n == 0 {
                    return Err::<ControlResponse, std::io::Error>(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the daemon closed the connection before answering",
                    ));
                }
                inbuf.extend_from_slice(&read[..n]);
                for incoming in connection.drain(&mut inbuf)? {
                    if let client::Incoming::Control(response) = incoming
                        && response.request_id() == id
                    {
                        return Ok(response);
                    }
                }
            }
        };
        // One bound over each whole exchange, connect and handshake included, not only
        // the answer: a daemon that accepts and never greets would otherwise hold the
        // agent's turn until the agent's own hook timeout, minutes for most of them.
        let bounded = || async {
            tokio::time::timeout(std::time::Duration::from_secs(5), attempt())
                .await
                .map_err(|_| std::io::Error::other("the daemon did not answer within 5s"))?
        };
        let answer = once_more_if_cut(bounded).await?;
        match answer {
            ControlResponse::AgentStateReported { .. } => {
                if let Some(session) = &kept {
                    qass::note_session(session);
                }
                Ok(())
            }
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(format!(
                "the daemon refused the report: {reason}"
            ))),
            other => Err(std::io::Error::other(format!(
                "unexpected answer to a hook report: {}",
                other.request_id()
            ))),
        }
    })
}

/// One exchange, made again once when the daemon cut it: moving onto a new binary closes every
/// connection it had accepted, and the next one serves the same socket once it is up. The
/// report goes again, with a bound of its own, rather than being lost; sent twice at worst,
/// when the first was taken and only its answer lost.
async fn once_more_if_cut<T, F, Fut>(attempt: F) -> std::io::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    match attempt().await {
        Err(e) if cut_by_daemon(&e) => {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            attempt().await
        }
        answer => answer,
    }
}

/// Whether the daemon cut the connection under a request: what it does to every connection it
/// had accepted when it moves onto a new binary, whose socket stays open across the move. A
/// hook report cut this way is sent once more; a refusal, no daemon at all, or a daemon that
/// never answers is not.
pub(crate) fn cut_by_daemon(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

/// `qoop reload-manifests`: re-read agent-detection overrides without restarting.
///
/// Prints what loaded and what did not. A bad file keeps the previous rules,
/// so this is safe to run blindly after editing the override directory.
fn run_reload_manifests() -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoop-cli").await?;
        let (mut rd, mut wr) = stream.into_split();
        let id = format!(
            "reload-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let mut seq = 0u32;
        wr.write_all(&client::encode_control_seq(
            &ControlRequest::ReloadAgentManifests { id: id.clone() },
            &mut seq,
        )?)
        .await?;
        let mut connection = client::Connection::new();
        let mut read = [0u8; 64 * 1024];
        let answer = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let n = rd.read(&mut read).await?;
                if n == 0 {
                    return Err::<ControlResponse, std::io::Error>(std::io::Error::other(
                        "the daemon closed the connection before answering",
                    ));
                }
                inbuf.extend_from_slice(&read[..n]);
                for incoming in connection.drain(&mut inbuf)? {
                    if let client::Incoming::Control(response) = incoming
                        && response.request_id() == id
                    {
                        return Ok(response);
                    }
                }
            }
        })
        .await
        .map_err(|_| std::io::Error::other("the daemon did not answer within 10s"))??;
        match answer {
            ControlResponse::ManifestsReloaded { loaded, errors, .. } => {
                for name in &loaded {
                    println!("loaded {name}");
                }
                for error in &errors {
                    println!("error {error}");
                }
                if loaded.is_empty() && errors.is_empty() {
                    println!("no overrides");
                }
                Ok(())
            }
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(format!(
                "the reload failed: {reason}"
            ))),
            other => Err(std::io::Error::other(format!(
                "unexpected answer to a reload: {}",
                other.request_id()
            ))),
        }
    })
}

/// Ask this session's daemon one control question and wait for its answer.
///
/// One place, because the three commands that ask one were three copies of the same
/// forty lines, and the copies had already drifted: two of them named the daemon's
/// silence differently and one of them read a reply meant for another request.
async fn ask_daemon(
    socket: &std::path::Path,
    client_id: &str,
    request: &qoop_runtime::control::ControlRequest,
) -> std::io::Result<qoop_runtime::control::ControlResponse> {
    let (stream, inbuf) = client::connect(socket, client_id).await?;
    ask_over(stream, inbuf, request, ASK_PATIENCE).await
}

/// The same question over a connection that has already done its handshake, a remote's
/// relay as well as a local socket, waiting at most `patience` for the answer, the write of
/// the question included.
///
/// A daemon that closes the connection first is `UnexpectedEof`, which for a move onto a
/// new binary may be the good answer: the daemon had nothing in the way and is going. A
/// daemon too old to know the move answers with no id, naming the ask it did not know (the
/// same case the interface reads, `mux.rs`); that is its answer too.
async fn ask_over<S>(
    stream: S,
    mut inbuf: bytes::BytesMut,
    request: &qoop_runtime::control::ControlRequest,
    patience: std::time::Duration,
) -> std::io::Result<qoop_runtime::control::ControlResponse>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use qoop_runtime::control::ControlResponse;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let id = request.request_id().to_string();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let mut seq = 0u32;
    let question = client::encode_control_seq(request, &mut seq)?;
    tokio::time::timeout(patience, async {
        wr.write_all(&question).await?;
        let mut connection = client::Connection::new();
        let mut read = [0u8; 64 * 1024];
        loop {
            let n = rd.read(&mut read).await?;
            if n == 0 {
                return Err::<ControlResponse, std::io::Error>(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the daemon closed the connection before answering",
                ));
            }
            inbuf.extend_from_slice(&read[..n]);
            for incoming in connection.drain(&mut inbuf)? {
                if let client::Incoming::Control(response) = incoming
                    && (response.request_id() == id
                        || matches!(&response, ControlResponse::Failed { id, reason }
                        if id.is_empty()
                            && ["RestartWhenSafe", "AskWrite", "AskRead", "ShowWrite"].iter().any(|variant| {
                                qoop_runtime::control::too_old_for(reason, Some(variant))
                            })))
                {
                    return Ok(response);
                }
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other(format!("the daemon did not answer within {patience:?}")))?
}

/// How long to wait for a stopped daemon to actually go. Past its own teardown patience,
/// because it ends its terminals before it ends itself.
const STOP_PATIENCE: std::time::Duration = std::time::Duration::from_secs(8);

/// How long any one control question waits. Long enough for a daemon busy with a hundred
/// terminals, short enough that a wedged one is reported rather than waited on.
const ASK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// A request id nothing else will use.
fn request_id(what: &str) -> String {
    format!(
        "{what}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// `qoop relay`: the far end of a remote attach.
///
/// Started over ssh, it connects to this machine's daemon socket and copies bytes between
/// that socket and its own stdin and stdout. The protocol does not change: a remote client
/// speaks exactly what a local one speaks, and the transport is a pipe rather than a
/// socket.
///
/// It starts the daemon if there is not one, for the same reason `qoop` does: a person
/// attaching to a machine expects a workspace, not an error telling them to log in and start
/// something first.
fn run_relay() -> std::io::Result<()> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    ensure_daemon(&socket)?;

    let stream = std::os::unix::net::UnixStream::connect(&socket)?;
    let mut to_daemon = stream.try_clone()?;
    let mut from_daemon = stream;

    // One thread each way. A relay is the one place in this program where two blocking
    // copies is the right shape: there is nothing else for it to do, and a runtime here
    // would be a runtime whose only job is to wait on two file descriptors.
    let up = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let _ = std::io::copy(&mut stdin, &mut to_daemon);
        // Half close, so the daemon sees the end of input rather than a stalled peer.
        let _ = to_daemon.shutdown(std::net::Shutdown::Write);
    });

    // Written out by hand rather than with `io::copy`, because stdout is line buffered and
    // a frame is binary with no newline in it. The welcome therefore sat in the buffer
    // until something later happened to flush it, which on a quiet workspace is never: the
    // far side saw silence, stopped acknowledging, and the daemon disconnected it thirty
    // seconds later as a wedged reader. Every chunk leaves as it arrives.
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match from_daemon.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if stdout.write_all(&buf[..n]).is_err() || stdout.flush().is_err() {
            break;
        }
    }
    // Not joined. Once the daemon side is closed nothing needs the stdin thread, and
    // waiting on it kept a dead relay up until the far side happened to send something,
    // with the link above still taking it for alive. When stdin ended first, that thread
    // already finished and half closed before the daemon's close reached us, so nothing
    // is lost by returning.
    drop(up);
    Ok(())
}

/// Send one control request and wait for the answer that echoes its id.
///
/// Renders that arrive while waiting are kept, not dropped: a fresh
/// connection is auto-subscribed to its lowest terminal, whose first screen
/// can arrive before the answer does. Dropping it would leave a reader of a
/// quiet terminal waiting for an update that never comes.
async fn control_roundtrip(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    inbuf: &mut bytes::BytesMut,
    connection: &mut client::Connection,
    seq: &mut u32,
    request: &qoop_runtime::control::ControlRequest,
    timeout: Duration,
) -> std::io::Result<(
    qoop_runtime::control::ControlResponse,
    Vec<client::Incoming>,
)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let want = request.request_id().to_string();
    wr.write_all(&client::encode_control_seq(request, seq)?)
        .await?;
    let mut read = [0u8; 64 * 1024];
    let mut stashed = Vec::new();
    let answer = tokio::time::timeout(timeout, async {
        loop {
            let n = rd.read(&mut read).await?;
            if n == 0 {
                return Err::<qoop_runtime::control::ControlResponse, std::io::Error>(
                    std::io::Error::other("the daemon closed the connection before answering"),
                );
            }
            inbuf.extend_from_slice(&read[..n]);
            // The whole batch is looked at before returning: what arrived in the same read
            // after the answer (a screen, a state change) is already out of the buffer, and
            // returning at the answer threw it away. A screen sent just after `Subscribed`
            // was lost that way, so `wait --match` missed text already on the screen.
            let mut answer = None;
            for incoming in connection.drain(inbuf)? {
                match incoming {
                    client::Incoming::Control(response)
                        if answer.is_none() && response.request_id() == want =>
                    {
                        answer = Some(response);
                    }
                    other => stashed.push(other),
                }
            }
            if let Some(response) = answer {
                return Ok(response);
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("the daemon did not answer in time"))??;
    Ok((answer, stashed))
}

/// Text or keys from another qoop terminal into qass's tab are refused: typed there they read
/// as the owner's own words, and one was typed into his half-written message and sent with it.
/// qass reads agents through their events, task and screen. Either variable set counts as a
/// terminal, so dropping one does not pass for the owner.
// ponytail: an agent that unsets both still types as him; the full fix is the daemon taking
// typed input into qass's terminal only from the interface, the phone and its own inbox.
fn into_qass_refusal(target: u32, cwd: impl FnOnce() -> Option<String>) -> Option<String> {
    let var = |k: &str| qoop_runtime::legacy::var(k).ok().filter(|v| !v.is_empty());
    let from = var("QOOP_TERMINAL");
    if from.is_none() && var("QOOP_TOKEN").is_none() {
        return None;
    }
    let is_qass = || {
        paths::qass_data_dir()
            .zip(cwd())
            .is_some_and(|(f, cwd)| crate::qass::in_folder(&cwd, &f))
    };
    if from.as_deref() == Some(target.to_string().as_str()) || !is_qass() {
        return None;
    }
    Some(
        "qass reads you through your task and your screen: say it there (`qoop task evidence`, \
         `qoop task stage waiting-human`) or in your reply; nothing was typed into qass"
            .to_string(),
    )
}

/// `qoop send`: type into another terminal's session.
///
/// This travels on the terminal channel's addressed input, the same lane every
/// keystroke uses, and adds no control method that takes bytes for a child.
/// That is why G7.1 keeps passing unchanged: the control codec still has no
/// write method. Same-user processes could already reach any terminal this
/// way; this only gives that path a name plus an existence check.
fn run_send(args: &[String]) -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};
    use tokio::io::AsyncWriteExt;

    let help = "usage: qoop send <terminal> [--enter] [--generation G] [--] <text...>\n       qoop send <terminal> --key NAME [--key NAME...] [--generation G]";
    let terminal: u32 = args
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| usage(help))?;
    let mut enter = false;
    let mut parts = Vec::new();
    let mut keys = Vec::new();
    let mut args = args.to_vec();
    let generation = supervise::take_generation(&mut args);
    let mut rest = args.iter().skip(3);
    while let Some(arg) = rest.next() {
        if arg == "--enter" {
            enter = true;
        } else if arg == "--key" {
            keys.push(rest.next().cloned().ok_or_else(|| usage(help))?);
        } else if arg == "--" {
            // Everything after is text, dashes and all: what a person typed is sent as typed.
            parts.extend(rest.by_ref().cloned());
        } else if arg.starts_with("--") {
            return Err(usage(format!("unknown flag {arg}; {help}")));
        } else {
            parts.push(arg.clone());
        }
    }
    // Keys and text in one call would have to agree on an order nobody wrote down.
    if !keys.is_empty() && (!parts.is_empty() || enter) {
        return Err(usage(format!(
            "send either text or --key, not both; {help}"
        )));
    }
    supervise::check_generation(terminal, generation)?;
    if let Some(reason) = into_qass_refusal(terminal, || {
        crate::qass::json(&["agents", "--json"])?
            .as_array()?
            .iter()
            .find(|r| r["terminal"].as_u64() == Some(u64::from(terminal)))?["cwd"]
            .as_str()
            .map(String::from)
    }) {
        return Err(std::io::Error::other(reason));
    }
    if !keys.is_empty() {
        supervise::send_keys(terminal, &keys)?;
        crate::qass::note_prompted(terminal);
        println!("sent to terminal {terminal}");
        return Ok(());
    }
    let mut text = parts.join(" ");
    if text.is_empty() {
        return Err(usage(format!("refusing to send empty input; {help}")));
    }
    if enter {
        text.push('\r');
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoop-send").await?;
        let (mut rd, mut wr) = stream.into_split();
        let mut connection = client::Connection::new();
        let mut seq = 0u32;
        let id = format!(
            "send-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        match control_roundtrip(
            &mut rd,
            &mut wr,
            &mut inbuf,
            &mut connection,
            &mut seq,
            &ControlRequest::ListTerminals { id },
            Duration::from_secs(10),
        )
        .await?
        .0
        {
            ControlResponse::Terminals { terminals, .. } if terminals.contains(&terminal) => {}
            _ => {
                return Err(std::io::Error::other(format!(
                    "no such terminal: {terminal}"
                )));
            }
        }
        let mut term_seq = 0u32;
        // Who sends, when the sender runs inside a terminal: binds the
        // daemon-side wait edge to its sender. Absent outside one.
        let (from_terminal, from_token) = cli_sender();
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes: text.into_bytes(),
                from_terminal,
                from_token,
            },
            &mut term_seq,
        )?)
        .await?;
        wr.flush().await?;
        crate::qass::note_prompted(terminal);
        println!("sent to terminal {terminal}");
        Ok(())
    })
}

/// Who this CLI call runs as, when it runs inside a qoop terminal: the
/// terminal number plus its token, for binding a send/prompt's
/// `from_terminal` claim to its sender. Both come from the environment the
/// daemon put around everything the session starts; a half claim (number
/// without token) sends nothing, and outside a terminal both are absent
/// and the daemon treats the input as unattributed, exactly like before.
fn cli_sender() -> (Option<u32>, Option<String>) {
    cli_sender_from(
        qoop_runtime::legacy::var("QOOP_TERMINAL").ok(),
        qoop_runtime::legacy::var("QOOP_TOKEN").ok(),
    )
}

/// `cli_sender` over explicit values, so the mapping is table-testable
/// without touching the process environment.
fn cli_sender_from(
    terminal: Option<String>,
    token: Option<String>,
) -> (Option<u32>, Option<String>) {
    let terminal: Option<u32> = terminal.and_then(|v| v.parse().ok());
    let token: Option<String> = token.filter(|v| !v.is_empty());
    match (terminal, token) {
        (Some(t), Some(tok)) => (Some(t), Some(tok)),
        _ => (None, None),
    }
}

/// Fetch one terminal's live screen: subscribe, settle, keep the last snapshot.
///
/// The same lane `run_read` uses. The daemon publishes the live screen on
/// subscribe, so settle briefly and keep the last snapshot rather than the
/// first. Shared by `read` and `context` so the two cannot disagree about
/// what "the screen" means.
async fn fetch_snapshot(
    terminal: u32,
    who: &str,
) -> std::io::Result<qoop_runtime::daemon::SnapshotWire> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};
    use tokio::io::AsyncReadExt;

    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    let (stream, mut inbuf) = client::connect(&socket, who).await?;
    let (mut rd, mut wr) = stream.into_split();
    let mut connection = client::Connection::new();
    let mut seq = 0u32;
    let id = format!(
        "read-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let (subscribed, mut stashed) = control_roundtrip(
        &mut rd,
        &mut wr,
        &mut inbuf,
        &mut connection,
        &mut seq,
        &ControlRequest::Subscribe {
            id,
            terminals: vec![terminal],
        },
        Duration::from_secs(10),
    )
    .await?;
    match subscribed {
        ControlResponse::Subscribed { terminals, .. } if terminals.contains(&terminal) => {}
        _ => {
            return Err(std::io::Error::other(format!(
                "no such terminal: {terminal}"
            )));
        }
    }
    let mut read = [0u8; 64 * 1024];
    // The first snapshot for the terminal can predate the subscription: output
    // printed while nobody watched was never published.
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut latest: Option<qoop_runtime::daemon::SnapshotWire> = None;
        let settle = std::time::Instant::now() + Duration::from_millis(500);
        for incoming in stashed.drain(..) {
            if let client::Incoming::Render {
                terminal: t,
                snapshot,
            } = incoming
                && t == terminal
            {
                latest = Some(snapshot);
            }
        }
        loop {
            for incoming in connection.drain(&mut inbuf)? {
                if let client::Incoming::Render {
                    terminal: t,
                    snapshot,
                } = incoming
                    && t == terminal
                {
                    latest = Some(snapshot);
                }
            }
            let now = std::time::Instant::now();
            if latest.is_some() && now >= settle {
                break;
            }
            if latest.is_none() {
                // Nothing yet: wait for the first snapshot, bounded by the
                // outer timeout rather than by the settle window.
                let n = rd.read(&mut read).await?;
                if n == 0 {
                    break;
                }
                inbuf.extend_from_slice(&read[..n]);
            } else {
                // Settling: wait at most the remaining window for a newer
                // snapshot, then keep what arrived last.
                match tokio::time::timeout(settle - now, rd.read(&mut read)).await {
                    Err(_) => break,
                    Ok(Err(e)) => return Err::<_, std::io::Error>(e),
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => inbuf.extend_from_slice(&read[..n]),
                }
            }
        }
        latest.ok_or_else(|| {
            std::io::Error::other("the daemon closed the connection before sending a snapshot")
        })
    })
    .await
    .map_err(|_| std::io::Error::other(format!("no output from terminal {terminal} within 10s")))?
}

/// Trim trailing blank rows, then keep the last N when asked.
fn trim_rows(mut rows: Vec<String>, lines: Option<usize>) -> Vec<String> {
    while rows.last().is_some_and(|l| l.trim().is_empty()) {
        rows.pop();
    }
    if let Some(n) = lines
        && rows.len() > n
    {
        rows = rows[rows.len() - n..].to_vec();
    }
    rows
}

/// `qoop read`: print another terminal's current screen.
///
/// Subscribes, takes the first snapshot for that terminal and prints it.
/// Trailing blank rows are trimmed; `--lines N` keeps the last N rows. `--json` prints
/// the screen as styled runs (see `mirror`), `--watch` prints it again on each change,
/// and `--back UP` reads history instead: `--lines` rows (200 when not said), the first
/// of them UP rows before the newest one written.
fn run_read(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop read <terminal> [--lines N] [--json] [--watch]\n       \
                qoop read <terminal> --back UP [--lines N] [--json]";
    let terminal: u32 = args
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| usage(help))?;
    let mut lines: Option<usize> = None;
    let mut back: Option<usize> = None;
    let (mut json, mut watch) = (false, false);
    let mut rest = args.iter().skip(3);
    while let Some(arg) = rest.next() {
        let mut number = || -> std::io::Result<usize> {
            rest.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| usage(help))
        };
        match arg.as_str() {
            "--lines" => lines = Some(number()?),
            "--back" => back = Some(number()?),
            "--json" => json = true,
            "--watch" => watch = true,
            _ => return Err(usage(format!("unknown flag {arg}; {help}"))),
        }
    }
    if let Some(up) = back {
        if watch {
            return Err(usage(format!("--back reads history once; {help}")));
        }
        let (from, total, rows) = supervise::read_back(terminal, up, lines.unwrap_or(200))?;
        if json {
            println!("{}", mirror::history(from, total, &rows));
        } else {
            for row in &rows {
                println!("{row}");
            }
        }
        return Ok(());
    }
    // A screen as JSON is every row, since a reader lines its runs up with the cursor and
    // the rows behind it; --lines is for the text form.
    if json && lines.is_some() {
        return Err(usage(format!(
            "--lines cuts the text screen; --json prints every row: {help}"
        )));
    }
    if watch {
        return supervise::run_read_watch(terminal, |snapshot| {
            Ok(if json {
                mirror::screen(snapshot).to_string()
            } else {
                trim_rows(snapshot.lines.clone(), lines).join("\n") + "\n"
            })
        });
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let snapshot = fetch_snapshot(terminal, "qoop-read").await?;
        if json {
            println!("{}", mirror::screen(&snapshot));
            return Ok(());
        }
        for row in &trim_rows(snapshot.lines, lines) {
            println!("{row}");
        }
        Ok(())
    })
}

/// `qoop focus <terminal> [--json]`: every attached interface shows that terminal.
///
/// `--json` answers `{"clients":[pid,...]}`, the interface processes that were asked,
/// which is what a notch or a launcher needs to raise the window one of them runs in.
fn run_focus(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop focus <terminal> [--json]";
    let terminal: u32 = args
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| usage(help))?;
    let mut json = false;
    for arg in args.iter().skip(3) {
        if arg == "--json" {
            json = true;
        } else {
            return Err(usage(format!("unknown flag {arg}; {help}")));
        }
    }
    let clients = supervise::run_focus(terminal)?;
    if json {
        println!("{}", serde_json::json!({ "clients": clients }));
    } else if clients.is_empty() {
        println!("no interface is attached, so nothing was shown");
    } else {
        println!(
            "terminal {terminal} shown in {} interface{}",
            clients.len(),
            if clients.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// Resolve a prompt target against the live index: a terminal number first,
/// then an exact terminal name, then an exact program name.
fn resolve_target<'a>(
    rows: &'a [qoop_runtime::control::AgentRow],
    target: &str,
) -> Result<&'a qoop_runtime::control::AgentRow, String> {
    if let Ok(n) = target.parse::<u32>()
        && let Some(row) = rows.iter().find(|r| r.terminal == n)
    {
        return Ok(row);
    }
    let by_name: Vec<_> = rows
        .iter()
        .filter(|r| r.name.as_deref() == Some(target))
        .collect();
    if by_name.len() == 1 {
        return Ok(by_name[0]);
    }
    if by_name.len() > 1 {
        return Err(format!("agent target {target} is ambiguous"));
    }
    let by_program: Vec<_> = rows
        .iter()
        .filter(|r| r.program.as_deref() == Some(target))
        .collect();
    if by_program.len() == 1 {
        return Ok(by_program[0]);
    }
    if by_program.len() > 1 {
        return Err(format!("agent target {target} is ambiguous"));
    }
    Err(format!("no such agent or terminal: {target}"))
}

/// Why a prompt must not be sent, if it must not be.
///
/// Pure, so the two refusals are tested without a daemon: a trust screen
/// eats bytes as a shell command rather than a task, and a blocked agent
/// answers a question nobody asked. Anything else is sendable, including a
/// shell at its prompt, which just runs the text as a command.
fn prompt_refusal(row: &qoop_runtime::control::AgentRow) -> Option<String> {
    let who = row
        .name
        .clone()
        .or_else(|| row.program.clone())
        .unwrap_or_else(|| format!("terminal {}", row.terminal));
    if row.readiness == "trust_required" {
        return Some(format!(
            "agent {who} is waiting on a trust prompt: answer it in the terminal first"
        ));
    }
    if row.state == "blocked" {
        return Some(format!(
            "agent {who} is blocked and requires interactive input"
        ));
    }
    if row.state == "sleeping" {
        return Some(format!(
            "terminal {who} is sleeping: it was reaped for idleness and takes no input"
        ));
    }
    None
}

/// `qoop prompt`: send text plus Enter to another session, unless it is blocked.
///
/// Not limited to agents: a
/// shell session accepts a prompt too, it just runs it as a command. The
/// blocked refusal stays, because typing over an interactive question is how a
/// "yes" lands on a question that was never asked. Text and Enter travel as
/// two writes with a short pause between them, so a bracketed paste lands
/// before the newline submits it.
///
/// `--wait` then waits for the turn the prompt caused (`supervise::Turn`), with the
/// exit status of `qoop wait`.
fn run_prompt(args: &[String]) -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};
    use tokio::io::AsyncWriteExt;

    let PromptOpts {
        target,
        text,
        wait,
        timeout,
        generation,
    } = parse_prompt_args(args).map_err(usage)?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoop-prompt").await?;
        let (mut rd, mut wr) = stream.into_split();
        let mut connection = client::Connection::new();
        let mut seq = 0u32;
        let id = format!(
            "prompt-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let rows = match control_roundtrip(
            &mut rd,
            &mut wr,
            &mut inbuf,
            &mut connection,
            &mut seq,
            &ControlRequest::ListAgents { id },
            Duration::from_secs(10),
        )
        .await?
        .0
        {
            ControlResponse::Agents { agents, .. } => agents,
            ControlResponse::Failed { reason, .. } => {
                return Err(std::io::Error::other(reason));
            }
            _ => {
                return Err(std::io::Error::other(
                    "unexpected answer to the agent listing",
                ));
            }
        };
        let row = resolve_target(&rows, &target).map_err(std::io::Error::other)?;
        if let Some(reason) = prompt_refusal(row) {
            return Err(std::io::Error::other(reason));
        }
        if let Some(reason) = into_qass_refusal(row.terminal, || row.cwd.clone()) {
            return Err(std::io::Error::other(reason));
        }
        if let Some(reason) =
            generation.and_then(|g| supervise::generation_refusal(&rows, row.terminal, g))
        {
            return Err(std::io::Error::other(reason));
        }
        let terminal = row.terminal;
        let mut term_seq = 0u32;
        // The Enter carries the sender, which records the wait edge.
        let (from_terminal, from_token) = cli_sender();
        // A paste, not typing: the daemon writes input in 1024-byte pieces, and an agent read
        // a long typed prompt as two pastes and kept only the second. Inside the markers it
        // asked for, the whole text is one paste however it is cut.
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Paste {
                bytes: text.into_bytes(),
            },
            &mut term_seq,
        )?)
        .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes: vec![b'\r'],
                from_terminal,
                from_token,
            },
            &mut term_seq,
        )?)
        .await?;
        wr.flush().await?;
        crate::qass::note_prompted(terminal);
        println!("prompted terminal {terminal}");
        if !wait {
            return Ok(());
        }
        let before = row.clone();
        // This connection is done: left open for the wait it would sit subscribed to the
        // lowest terminal without acknowledging, counting as a watcher of it until the
        // daemon dropped it as wedged at thirty seconds.
        drop(rd);
        drop(wr);
        supervise::finish_turn(supervise::wait_for_turn(&before, timeout).await?)
    })
}

/// Parsed `qoop prompt` arguments, pure so the refusals are testable.
struct PromptOpts {
    target: String,
    text: String,
    wait: bool,
    timeout: Option<Duration>,
    generation: Option<u64>,
}

/// Flags right after the target, up to an optional `--`: `--wait`, `--timeout SECS`,
/// `--generation G`. The first word that is not one starts the text, and everything from
/// there is text, except a trailing `--generation G`, which `send` and `close` take the
/// same way. A text that ends in `--wait` or `--timeout N` is refused as a misplaced flag
/// rather than typed into the agent, unless it follows `--`.
fn parse_prompt_args(args: &[String]) -> Result<PromptOpts, String> {
    let help = "usage: qoop prompt <target> [--wait] [--timeout SECS] [--generation G] [--] <text...> [--generation G]";
    let mut args = args.to_vec();
    let mut generation = supervise::take_generation(&mut args);
    let target = args.get(2).cloned().ok_or_else(|| help.to_string())?;
    let mut wait = false;
    let mut timeout: Option<Duration> = None;
    let mut literal = false;
    let mut rest = args.iter().skip(3).peekable();
    // A word with whitespace in it is text even when it starts with dashes: `"--- notes"`
    // and `"--resume please"` are one quoted argument, never a flag.
    while let Some(arg) =
        rest.next_if(|a| a.starts_with("--") && !a.chars().any(char::is_whitespace))
    {
        match arg.as_str() {
            "--" => {
                literal = true;
                break;
            }
            "--wait" => wait = true,
            "--timeout" => {
                let secs = rest.next().and_then(|v| v.parse::<u64>().ok());
                timeout = Some(Duration::from_secs(secs.ok_or_else(|| help.to_string())?));
            }
            "--generation" => {
                let value = rest.next().and_then(|v| v.parse::<u64>().ok());
                if generation.is_some() {
                    return Err(format!("--generation given twice; {help}"));
                }
                generation = Some(value.ok_or_else(|| help.to_string())?);
            }
            other => return Err(format!("unknown flag {other}; {help}")),
        }
    }
    if timeout.is_some() && !wait {
        return Err(format!("--timeout needs --wait; {help}"));
    }
    let words: Vec<&str> = rest.map(String::as_str).collect();
    let trailing = words.last() == Some(&"--wait")
        || (words.len() >= 2 && words[words.len() - 2] == "--timeout");
    if trailing && !literal {
        return Err(format!(
            "flags go right after the target: qoop prompt <target> --wait <text>; {help}"
        ));
    }
    let text = words.join(" ");
    if text.is_empty() {
        return Err(format!("refusing to send an empty prompt; {help}"));
    }
    Ok(PromptOpts {
        target,
        text,
        wait,
        timeout,
        generation,
    })
}

/// `qoop history`: print another agent's recent transcript window.
///
/// Resolution reuses `resolve_target` and the single `ListAgents` round
/// trip, like `prompt`. The transcript itself is read from the agent
/// product's own store by `qoop-history`, never through the daemon: local
/// files only, newest last. Every refusal names the product and the id (or
/// the missing hook report), never a filesystem path.
fn run_history(args: &[String]) -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};

    if args.iter().skip(2).any(|a| a == "--dir") {
        return run_history_dir(args);
    }
    let opts = parse_history_args(args).map_err(usage)?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoop-history").await?;
        let (mut rd, mut wr) = stream.into_split();
        let mut connection = client::Connection::new();
        let mut seq = 0u32;
        let id = format!(
            "history-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let agents = match control_roundtrip(
            &mut rd,
            &mut wr,
            &mut inbuf,
            &mut connection,
            &mut seq,
            &ControlRequest::ListAgents { id },
            Duration::from_secs(10),
        )
        .await?
        .0
        {
            ControlResponse::Agents { agents, .. } => agents,
            ControlResponse::Failed { reason, .. } => {
                return Err(std::io::Error::other(reason));
            }
            _ => {
                return Err(std::io::Error::other(
                    "unexpected answer to the agent listing",
                ));
            }
        };
        // The listing is the only reader of the registry, so history filters
        // it rather than asking a second question: two answers to "what is
        // there" would be two answers.
        let (program, session) = match resolve_target(&agents, &opts.target) {
            Ok(row) => (
                row.program.clone().unwrap_or_default(),
                row.agent_session.clone(),
            ),
            Err(why) => sleeping_agent(&opts.target).ok_or_else(|| std::io::Error::other(why))?,
        };
        let base = qoop_history::BaseDirs::from_home()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let window =
            qoop_history::history(&program, session.as_deref(), &base, opts.last, opts.role)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        if opts.json {
            println!(
                "{}",
                serde_json::to_string(&window).map_err(std::io::Error::other)?
            );
        } else {
            let rendered = render_window_text(&window);
            eprint!("{}", rendered.stderr);
            print!("{}", rendered.stdout);
        }
        Ok(())
    })
}

/// `qoop history --dir <repo>`: what the person typed to their agents in a repository and
/// its worktrees, across every session there, oldest first. No daemon: only the products'
/// own stores are read.
fn run_history_dir(args: &[String]) -> std::io::Result<()> {
    let opts = parse_history_dir_args(args).map_err(usage)?;
    let repo = worktree::repo_of(&opts.dir)?;
    let base =
        qoop_history::BaseDirs::from_home().map_err(|e| std::io::Error::other(e.to_string()))?;
    let qoop_history::TypedIn {
        messages: typed,
        unread,
    } = qoop_history::typed_in(&repo, &base, opts.last);
    for gap in unread {
        eprintln!("qoop: not read: {gap}");
    }
    if opts.json {
        println!(
            "{}",
            serde_json::to_string(&typed).map_err(std::io::Error::other)?
        );
    } else {
        print!("{}", render_typed(&typed));
    }
    Ok(())
}

/// Parsed `qoop history --dir` arguments.
struct HistoryDirOpts {
    dir: PathBuf,
    last: usize,
    json: bool,
}

/// A repository's history reads at most this many messages per call: enough to learn how
/// a person works there, small enough to read.
const HISTORY_DIR_LAST_MAX: usize = 2_000;

fn parse_history_dir_args(args: &[String]) -> Result<HistoryDirOpts, String> {
    let usage = "usage: qoop history --dir <repo> [--last N] [--json]";
    let mut dir = None;
    let mut last = 200usize;
    let mut json = false;
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(rest.next().ok_or_else(|| usage.to_string())?)),
            "--last" => {
                let n = rest.next().ok_or_else(|| usage.to_string())?;
                last = n.parse::<usize>().map_err(|_| usage.to_string())?;
                if last == 0 || last > HISTORY_DIR_LAST_MAX {
                    return Err(usage.to_string());
                }
            }
            "--json" => json = true,
            _ => return Err(format!("unknown argument {arg}; {usage}")),
        }
    }
    Ok(HistoryDirOpts {
        dir: dir.ok_or_else(|| usage.to_string())?,
        last,
        json,
    })
}

/// One line per message: day, product, session, and the text on one line, cut to 400
/// characters, so a long history stays something to read and grep.
fn render_typed(typed: &[qoop_history::Typed]) -> String {
    let mut out = String::new();
    for m in typed {
        let flat = m.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let cut: String = flat.chars().take(400).collect();
        let more = if cut.len() < flat.len() { "…" } else { "" };
        out.push_str(&format!(
            "{} {} {}: {cut}{more}\n",
            qoop_history::date_utc(m.ts),
            m.product.as_str(),
            m.session
        ));
    }
    out
}

/// Parsed `qoop history` arguments, pure so the refusals are testable.
struct HistoryOpts {
    target: String,
    last: usize,
    role: Option<qoop_history::Role>,
    json: bool,
}

/// History reads at most this many messages per call.
const HISTORY_LAST_MAX: usize = 200;

fn parse_history_args(args: &[String]) -> Result<HistoryOpts, String> {
    let usage = "usage: qoop history <target> [--last N] [--role user|assistant] [--json]";
    let target = args.get(2).cloned().ok_or_else(|| usage.to_string())?;
    let mut last = 20usize;
    let mut role = None;
    let mut json = false;
    let mut rest = args.iter().skip(3).peekable();
    while let Some(arg) = rest.next() {
        if arg == "--last" {
            let n = rest.next().ok_or_else(|| usage.to_string())?;
            last = n.parse::<usize>().map_err(|_| usage.to_string())?;
            if last == 0 || last > HISTORY_LAST_MAX {
                return Err(usage.to_string());
            }
        } else if arg == "--role" {
            let r = rest.next().ok_or_else(|| usage.to_string())?;
            role = Some(match r.as_str() {
                "user" => qoop_history::Role::User,
                "assistant" => qoop_history::Role::Assistant,
                _ => return Err(usage.to_string()),
            });
        } else if arg == "--json" {
            json = true;
        } else {
            return Err(format!("unknown flag {arg}; {usage}"));
        }
    }
    Ok(HistoryOpts {
        target,
        last,
        role,
        json,
    })
}

/// Text rendering of a window, pure so the pin line, the skipped warning
/// and the truncation footnote are testable without a daemon.
struct TextRender {
    stdout: String,
    stderr: String,
}

fn render_window_text(window: &qoop_history::Window) -> TextRender {
    // The pin travels in --json inside the schema; text mode names it up
    // front so the output is attributable without it.
    let mut stdout = format!(
        "session product={} id={}\n",
        window.session.product.as_str(),
        window.session.id
    );
    for m in &window.messages {
        let role = match m.role {
            qoop_history::Role::User => "user",
            qoop_history::Role::Assistant => "assistant",
        };
        stdout.push_str(&format!("{role}: {}\n", truncate_text(&m.text)));
    }
    let mut stderr = String::new();
    if window.skipped_lines > 0 {
        stderr.push_str(&format!(
            "warning: skipped {} corrupt transcript lines\n",
            window.skipped_lines
        ));
    }
    if window.truncated {
        stderr.push_str("(older messages were not scanned)\n");
    }
    TextRender { stdout, stderr }
}

/// Cap one message for text output, marked. `--json` stays full fidelity.
fn truncate_text(text: &str) -> String {
    const CAP: usize = 4000;
    for (count, (i, _)) in text.char_indices().enumerate() {
        if count == CAP {
            return format!("{}[…truncated]", &text[..i]);
        }
    }
    text.to_string()
}

/// `qoop context`: understand another agent before writing to it.
///
/// One `ListAgents` round trip resolves the target and answers who it is
/// and whether it is writable; the live screen comes down the same lane
/// `read` uses; the transcript window is read from the agent product's own
/// store like `history`, local files only. Read-only composition: no new
/// control method, no bytes to any child.
///
/// The transcript section never hard-fails the command: a terminal with no
/// reported session id still shows its row, its screen and its writability,
/// with the history refusal as a line rather than an exit status.
fn run_context(args: &[String]) -> std::io::Result<()> {
    use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse};

    let opts = parse_context_args(args).map_err(usage)?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoop-context").await?;
        let (mut rd, mut wr) = stream.into_split();
        let mut connection = client::Connection::new();
        let mut seq = 0u32;
        let id = format!(
            "context-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let agents = match control_roundtrip(
            &mut rd,
            &mut wr,
            &mut inbuf,
            &mut connection,
            &mut seq,
            &ControlRequest::ListAgents { id },
            Duration::from_secs(10),
        )
        .await?
        .0
        {
            ControlResponse::Agents { agents, .. } => agents,
            ControlResponse::Failed { reason, .. } => {
                return Err(std::io::Error::other(reason));
            }
            _ => {
                return Err(std::io::Error::other(
                    "unexpected answer to the agent listing",
                ));
            }
        };
        let row: AgentRow = resolve_target(&agents, &opts.target)
            .map_err(std::io::Error::other)?
            .clone();
        let refusal = prompt_refusal(&row);
        let screen = trim_rows(
            fetch_snapshot(row.terminal, "qoop-context").await?.lines,
            opts.lines,
        );
        let program = row.program.clone().unwrap_or_default();
        let history: Result<qoop_history::Window, String> = qoop_history::BaseDirs::from_home()
            .map_err(|e| e.to_string())
            .and_then(|base| {
                qoop_history::history(
                    &program,
                    row.agent_session.as_deref(),
                    &base,
                    opts.last,
                    opts.role,
                )
                .map_err(|e| e.to_string())
            });
        if opts.json {
            println!(
                "{}",
                serde_json::to_string(&ContextJson::new(
                    &row,
                    refusal.as_deref(),
                    &screen,
                    history
                ))
                .map_err(std::io::Error::other)?
            );
        } else {
            print!(
                "{}",
                render_context_text(&row, refusal.as_deref(), &screen, history)
            );
        }
        Ok(())
    })
}

/// Parsed `qoop context` arguments, pure so the refusals are testable.
struct ContextOpts {
    target: String,
    last: usize,
    role: Option<qoop_history::Role>,
    lines: Option<usize>,
    json: bool,
}

fn parse_context_args(args: &[String]) -> Result<ContextOpts, String> {
    let usage =
        "usage: qoop context <target> [--last N] [--role user|assistant] [--lines N] [--json]";
    let target = args.get(2).cloned().ok_or_else(|| usage.to_string())?;
    let mut last = 20usize;
    let mut role = None;
    let mut lines: Option<usize> = None;
    let mut json = false;
    let mut rest = args.iter().skip(3).peekable();
    while let Some(arg) = rest.next() {
        if arg == "--last" {
            let n = rest.next().ok_or_else(|| usage.to_string())?;
            last = n.parse::<usize>().map_err(|_| usage.to_string())?;
            if last == 0 || last > HISTORY_LAST_MAX {
                return Err(usage.to_string());
            }
        } else if arg == "--role" {
            let r = rest.next().ok_or_else(|| usage.to_string())?;
            role = Some(match r.as_str() {
                "user" => qoop_history::Role::User,
                "assistant" => qoop_history::Role::Assistant,
                _ => return Err(usage.to_string()),
            });
        } else if arg == "--lines" {
            lines = Some(
                rest.next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| usage.to_string())?,
            );
        } else if arg == "--json" {
            json = true;
        } else {
            return Err(format!("unknown flag {arg}; {usage}"));
        }
    }
    Ok(ContextOpts {
        target,
        last,
        role,
        lines,
        json,
    })
}

/// Machine form of one context: the row subset an agent polls, writability,
/// the trimmed screen and either the transcript window or the refusal.
#[derive(serde::Serialize)]
struct ContextJson<'a> {
    terminal: u32,
    workspace: i64,
    name: Option<&'a str>,
    program: Option<&'a str>,
    state: &'a str,
    readiness: &'a str,
    agent: bool,
    agent_session: Option<&'a str>,
    agent_title: Option<&'a str>,
    generation: Option<u64>,
    source: Option<&'a str>,
    cwd: Option<&'a str>,
    last_active_at_unix: Option<i64>,
    writable: bool,
    refusal: Option<&'a str>,
    screen: &'a [String],
    #[serde(flatten)]
    history: ContextHistory,
}

#[derive(serde::Serialize)]
#[serde(untagged)]
enum ContextHistory {
    Window { history: qoop_history::Window },
    Refused { history_error: String },
}

impl<'a> ContextJson<'a> {
    fn new(
        row: &'a qoop_runtime::control::AgentRow,
        refusal: Option<&'a str>,
        screen: &'a [String],
        history: Result<qoop_history::Window, String>,
    ) -> Self {
        Self {
            terminal: row.terminal,
            workspace: row.workspace,
            name: row.name.as_deref(),
            program: row.program.as_deref(),
            state: &row.state,
            readiness: &row.readiness,
            agent: row.agent,
            agent_session: row.agent_session.as_deref(),
            agent_title: row.agent_title.as_deref(),
            generation: row.generation,
            source: row.source.as_deref(),
            cwd: row.cwd.as_deref(),
            last_active_at_unix: row.last_active_at_unix,
            writable: refusal.is_none(),
            refusal,
            screen,
            history: match history {
                Ok(history) => ContextHistory::Window { history },
                Err(history_error) => ContextHistory::Refused { history_error },
            },
        }
    }
}

/// Human form of one context: who, writable, screen, transcript.
fn render_context_text(
    row: &qoop_runtime::control::AgentRow,
    refusal: Option<&str>,
    screen: &[String],
    history: Result<qoop_history::Window, String>,
) -> String {
    let mut out = String::new();
    let who = row
        .name
        .clone()
        .or_else(|| row.program.clone())
        .unwrap_or_else(|| format!("terminal {}", row.terminal));
    out.push_str(&format!(
        "terminal={} workspace={} who={} state={} readiness={} session={} cwd={}\n",
        row.terminal,
        row.workspace,
        who,
        row.state,
        row.readiness,
        row.agent_session.as_deref().unwrap_or("-"),
        row.cwd.as_deref().unwrap_or("-"),
    ));
    match refusal {
        None => out.push_str("writable=yes\n"),
        Some(reason) => out.push_str(&format!("writable=no ({reason})\n")),
    }
    out.push_str("--- screen ---\n");
    for line in screen {
        out.push_str(line);
        out.push('\n');
    }
    match history {
        Ok(window) => {
            let rendered = render_window_text(&window);
            if !rendered.stderr.is_empty() {
                out.push_str(&rendered.stderr);
                if !rendered.stderr.ends_with('\n') {
                    out.push('\n');
                }
            }
            out.push_str("--- history ---\n");
            out.push_str(&rendered.stdout);
        }
        Err(e) => {
            out.push_str(&format!("--- history: {e}\n"));
        }
    }
    out
}

/// `qoop agents`: the live index, one row per terminal.
///
/// Read only: one `ListAgents` round trip, no subscription, no input. The
/// text form is one `key=value` row per terminal for a person skimming; the
/// `--json` form is the same rows as an array for an agent polling. Both
/// come from the same response, so they cannot disagree.
fn run_agents(args: &[String]) -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};

    let help = "usage: qoop agents [--json]\n       qoop agents --watch [--all] [--json]\n       qoop agents explain <target> [--json]";
    enum Mode<'a> {
        List { json: bool },
        Explain { target: &'a str, json: bool },
    }
    let mut mode = Mode::List { json: false };
    let mut watch = false;
    let mut all = false;
    let mut rest = args.iter().skip(2);
    if rest.clone().next().is_some_and(|a| a == "explain") {
        rest.next();
        let Some(target) = rest.next() else {
            return Err(usage(help));
        };
        let mut json = false;
        for arg in rest {
            if arg == "--json" {
                json = true;
            } else {
                return Err(usage(format!("unknown flag {arg}; {help}")));
            }
        }
        mode = Mode::Explain { target, json };
    } else {
        for arg in rest {
            if arg == "--json" {
                mode = Mode::List { json: true };
            } else if arg == "--watch" {
                watch = true;
            } else if arg == "--all" {
                all = true;
            } else {
                return Err(usage(format!("unknown flag {arg}; {help}")));
            }
        }
    }
    if all && !watch {
        return Err(usage(format!("--all goes with --watch; {help}")));
    }
    if watch {
        let json = matches!(mode, Mode::List { json: true });
        let line = move |sessions: &supervise::Sessions| watch_listing(sessions, json, all);
        // One daemon, so `--all` is the same watch with each row naming its session, which
        // is what a notch built for many sessions reads.
        return supervise::run_watch(line);
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = match client::connect(&socket, "qoop-agents").await {
            Ok(connected) => connected,
            // No daemon is nothing awake, which is an answer: an empty list. An
            // explanation of one agent still needs that agent, and says so.
            Err(e)
                if matches!(mode, Mode::List { .. })
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
            {
                if matches!(mode, Mode::List { json: true }) {
                    println!("[]");
                }
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let (mut rd, mut wr) = stream.into_split();
        let mut connection = client::Connection::new();
        let mut seq = 0u32;
        let id = format!(
            "agents-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let agents = match control_roundtrip(
            &mut rd,
            &mut wr,
            &mut inbuf,
            &mut connection,
            &mut seq,
            &ControlRequest::ListAgents { id },
            Duration::from_secs(10),
        )
        .await?
        .0
        {
            ControlResponse::Agents { agents, .. } => agents,
            ControlResponse::Failed { reason, .. } => {
                return Err(std::io::Error::other(reason));
            }
            _ => {
                return Err(std::io::Error::other(
                    "unexpected answer to the agent listing",
                ));
            }
        };
        match mode {
            Mode::List { json } => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&agents).map_err(std::io::Error::other)?
                    );
                } else {
                    for row in &agents {
                        println!("{}", agent_line(row));
                    }
                }
            }
            Mode::Explain { target, json } => {
                // The listing is the only reader of the registry, so the
                // explanation filters it rather than asking a second
                // question: two answers to "what is there" would be two
                // answers.
                let row = resolve_target(&agents, target).map_err(std::io::Error::other)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(row).map_err(std::io::Error::other)?
                    );
                } else {
                    print!("{}", explain_row(row));
                }
            }
        }
        Ok(())
    })
}

/// One terminal on one line, for a person skimming `qoop agents`.
///
/// `key=value` rather than columns, because a chosen name can hold a space
/// and columns would then stop lining up exactly when they are needed.
/// One listing of a watch. With `all`, every row names its session: as a `session` field
/// in JSON, as a first word in text.
fn watch_listing(sessions: &supervise::Sessions, json: bool, all: bool) -> std::io::Result<String> {
    let rows = sessions
        .iter()
        .flat_map(|(name, rows)| rows.iter().map(move |row| (name, row)));
    if json {
        let rows = rows
            .map(|(name, row)| {
                let mut value = serde_json::to_value(row)?;
                if all && let Some(fields) = value.as_object_mut() {
                    fields.insert("session".to_string(), name.as_str().into());
                }
                Ok(value)
            })
            .collect::<serde_json::Result<Vec<_>>>()
            .map_err(std::io::Error::other)?;
        return serde_json::to_string(&rows).map_err(std::io::Error::other);
    }
    // A blank line between listings, so a person reading along sees where one ends.
    let lines: Vec<String> = rows
        .map(|(name, row)| match all {
            true => format!("session={name} {}", agent_line(row)),
            false => agent_line(row),
        })
        .collect();
    Ok(lines.join("\n") + "\n")
}

fn agent_line(row: &qoop_runtime::control::AgentRow) -> String {
    let program = row.program.as_deref().unwrap_or("-");
    let name = row.name.as_deref().unwrap_or("-");
    let generation = row
        .generation
        .map(|g| g.to_string())
        .unwrap_or_else(|| "-".to_string());
    let background = match row.background {
        0 => String::new(),
        n => format!(" background={n}"),
    };
    let subagents = match row.subagents {
        0 => String::new(),
        n => format!(" subagents={n}"),
    };
    format!(
        "term={} state={} program={} name={} workspace={} generation={}{background}{subagents}",
        row.terminal, row.state, program, name, row.workspace, generation
    )
}

/// Why one terminal reads the way it does, for `qoop agents explain`.
///
/// The belief first (state plus the source that holds it), then what the
/// screen last matched in this generation. A missing rule is an answer, not
/// a gap: it says the screen matched nothing since the generation began.
fn explain_row(row: &qoop_runtime::control::AgentRow) -> String {
    let program = row.program.as_deref().unwrap_or("-");
    let generation = row
        .generation
        .map(|g| g.to_string())
        .unwrap_or_else(|| "-".to_string());
    let source = row.source.as_deref().unwrap_or("unobserved");
    let rule = row.rule.as_deref().unwrap_or("-");
    let manifest = row.manifest.as_deref().unwrap_or("-");
    let mut text = format!(
        "terminal={} state={} source={} program={} generation={} readiness={} workspace={}\nrule={} manifest={}\n",
        row.terminal,
        row.state,
        source,
        program,
        generation,
        row.readiness,
        row.workspace,
        rule,
        manifest,
    );
    // Only when something was thrown away: a screen missing a sequence, a picture or some
    // scrollback can read wrong, and this is where a person asking why finds out.
    if let Some(d) = row.discards {
        text.push_str(&format!(
            "discarded: control_strings={} ({} bytes) images={} ({} bytes) images_flooded={} \
             evicted_unobserved={}\n",
            d.control_strings,
            d.control_string_bytes,
            d.images,
            d.image_bytes,
            d.images_flooded,
            d.evicted_unobserved,
        ));
    }
    text
}

/// The phone commands. Built from this repository alone there is no phone part: a bridge
/// reload (what install and update run) has nothing to put back, and the rest says so. A
/// `qoop bridge` that a release build's service still runs fails, so its log says why.
fn phone_command(phone: Option<Phone>, args: &[String]) -> std::io::Result<()> {
    if let Some(phone) = phone {
        return phone(args);
    }
    if args[1..] == ["bridge", "reload"] {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "`qoop {}` is part of the qoop release; this build has no phone support",
        args[1]
    )))
}

/// Start the daemon if there is not one, and wait for its socket.
pub(crate) fn ensure_daemon(socket: &std::path::Path) -> std::io::Result<()> {
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let spawned = std::time::SystemTime::now();
    // Its own process group, so the hangup a closing window or a dropped ssh connection
    // sends the starter's job does not take the daemon, and every tab, with it.
    Command::new(exe)
        .arg("daemon")
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Only a reason written by the daemon just started, not one left by an earlier start.
    let failed = socket.with_file_name("qoopd.failed");
    let fresh = std::fs::metadata(&failed)
        .and_then(|m| m.modified())
        .is_ok_and(|at| at >= spawned);
    if fresh && let Ok(why) = std::fs::read_to_string(&failed) {
        return Err(std::io::Error::other(format!(
            "the daemon did not start: {why}"
        )));
    }
    Err(std::io::Error::other(format!(
        "the daemon did not start; check {}",
        socket.display()
    )))
}

/// Pull `--session <name>` out of the arguments, wherever it was written.
///
/// Removed rather than left in place, so every command's own argument parsing sees the
/// arguments it expects. Accepted only as `default`: that is what an older qoop on the
/// other end of ssh sends, and any other name belonged to a session that is a space now,
/// refused with where its files went.
fn take_session(args: &mut Vec<String>) -> Result<(), paths::PathError> {
    let mut at = 1;
    while at < args.len() {
        // No short `-s`: the flag is taken from anywhere in argv, text words of `send`
        // and `prompt` included, and `du -s` or `ls -s` there is text, not a session.
        // Past a `--` every word is text, so `send 3 -- --session x` types it.
        if args[at] == "--" {
            break;
        }
        let taken = if args[at] == "--session" {
            let value = args.get(at + 1).cloned().unwrap_or_default();
            args.drain(at..(at + 2).min(args.len()));
            Some(value)
        } else if let Some(value) = args[at].strip_prefix("--session=") {
            let value = value.to_string();
            args.remove(at);
            Some(value)
        } else {
            at += 1;
            None
        };
        if let Some(value) = taken
            && value != paths::DEFAULT_SESSION
        {
            // An empty value is a flag with nothing after it, a mistake, not the default.
            let base = paths::runtime_base()?;
            if value.is_empty() {
                return paths::only_default("--session", &base);
            }
            return paths::only_default(&value, &base);
        }
    }
    Ok(())
}

/// The store as it is on disk: where the daemon keeps it, or beside the socket when no
/// daemon of this build has started yet to move it. Never created, so a reader never leaves
/// an empty database behind for the next daemon to take as the real one; a file gone
/// between the look and the open is an error to report.
fn existing_store(dir: &std::path::Path) -> std::io::Result<Option<qoop_store::Store>> {
    [paths::state_location(dir), dir.to_path_buf()]
        .into_iter()
        .map(|d| qoop_runtime::legacy::db_path(&d))
        // A missing or stray file is "no store yet", and the next place may hold the real one.
        .find(|db| qoop_store::Store::holds_schema(db))
        .map(|db| qoop_store::Store::open_existing(&db).map_err(std::io::Error::other))
        .transpose()
}

/// The agent a sleeping tab holds, `(program, its conversation id)`, from the store: a
/// tab that slept under an earlier daemon has no row in the live listing, yet its
/// conversation can still be read.
fn sleeping_agent(target: &str) -> Option<(String, Option<String>)> {
    let terminal: i64 = target.parse().ok()?;
    let dir = paths::runtime_dir().ok()?;
    existing_store(&dir).ok()??.sleeping_agent(terminal).ok()?
}

/// `qoop ls`: this machine's spaces, and which of them are asleep.
///
/// Read off the store rather than from a daemon when none is running, which is when the
/// question is most often asked: a daemon that left because nothing was awake still has
/// every space and tab written down.
fn run_list_spaces(args: &[String]) -> std::io::Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let running = std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_ok();
    let Some(store) = existing_store(&dir)? else {
        println!(
            "{}",
            if json {
                "[]"
            } else {
                "no spaces yet. `qoop` starts one."
            }
        );
        return Ok(());
    };
    let asleep = store.asleep().map_err(std::io::Error::other)?;
    let mut rows = Vec::new();
    for space in store.workspaces().map_err(std::io::Error::other)? {
        let tabs = store
            .open_terminals(space.id)
            .map_err(std::io::Error::other)?
            .len();
        let mut sleeping: Vec<i64> = asleep
            .iter()
            .filter(|(_, workspace, _)| *workspace == space.id)
            .map(|(terminal, _, _)| *terminal)
            .collect();
        sleeping.sort_unstable();
        // With no daemon nothing runs: a tab not marked asleep comes back awake on the next
        // start, and is said as stopped until then.
        let state = if tabs > 0 && sleeping.len() == tabs {
            "asleep".to_string()
        } else if !sleeping.is_empty() {
            format!("{} of {tabs} asleep", sleeping.len())
        } else if running {
            "awake".to_string()
        } else {
            "stopped".to_string()
        };
        rows.push((space.id, space.name, state, tabs, sleeping, space.goal));
    }
    if json {
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|(id, name, state, tabs, sleeping, goal)| {
                serde_json::json!({
                    "id": id, "name": name, "state": state, "tabs": tabs, "sleeping": sleeping,
                    "goal": goal
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(list));
        return Ok(());
    }
    if rows.is_empty() {
        println!("no spaces yet. `qoop` starts one.");
    }
    // Never a control character: a name or a goal with an escape in it would reach this
    // terminal as a command (retitle it, write the clipboard).
    let drawn =
        |s: String| -> String { s.chars().filter(|c| qoop_tui::width::drawn(*c)).collect() };
    for (id, name, state, tabs, sleeping, goal) in rows {
        let (name, goal) = (drawn(name), goal.map(drawn));
        let tabs = count(tabs, "tab");
        let sleeping = if sleeping.is_empty() {
            String::new()
        } else {
            let numbers: Vec<String> = sleeping.iter().map(|t| t.to_string()).collect();
            format!("  asleep: {}", numbers.join(" "))
        };
        // A long name is cut, so the columns after it stay in place, and said whole under its
        // row, so two that begin alike stay apart and either can be typed to `qoop sleep`.
        let shown = crate::settings::column(&name, 20);
        println!("{id:>3}  {shown} {state:<14} {tabs}{sleeping}");
        if qoop_tui::width::width(&name) > 20 {
            println!("     name: {name}");
        }
        if let Some(goal) = goal {
            println!("     goal: {goal}");
        }
    }
    Ok(())
}

/// `1 tab`, `2 tabs`: a count and its word, the word plural for any count but one.
pub(crate) fn count(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// `qoop stop`: end the daemon, and with it everything it was running.
///
/// The tabs are not lost by this and are not meant to be: what was open stays recorded, so
/// the next `qoop` brings back the same spaces, the same tabs, the same directories, and
/// carries on the conversation of any agent that was in one, and a space that was asleep
/// stays asleep. What does not survive is the processes, and that is not a choice: a
/// terminal is a pty this daemon owns, and its children get a hangup when it goes.
fn run_stop(args: &[String]) -> std::io::Result<()> {
    // `qoop stop work` named a session once; there is one daemon now, so a name other than
    // the default is pointed at where that session's files are. A daemon of that name still
    // running from before is stopped by it, since nothing else reaches it now.
    let dir = match args.get(2) {
        Some(named) => {
            let base = paths::runtime_base().map_err(std::io::Error::other)?;
            match paths::only_default(named, &base) {
                Ok(()) => paths::runtime_dir().map_err(std::io::Error::other)?,
                Err(paths::PathError::NamedSession { dir, .. })
                    if std::os::unix::net::UnixStream::connect(paths::socket_path(&dir))
                        .is_ok() =>
                {
                    dir
                }
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
        None => paths::runtime_dir().map_err(std::io::Error::other)?,
    };
    let socket = paths::socket_path(&dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        println!("qoop is not running");
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        use qoop_runtime::control::{ControlRequest, ControlResponse};

        let answer = ask_daemon(
            &socket,
            "qoop-stop",
            &ControlRequest::Shutdown {
                id: request_id("stop"),
            },
        )
        .await?;
        match answer {
            // The daemon answers before it goes, so a person is told it heard rather than
            // being left to guess at a socket that stopped answering.
            ControlResponse::Stopping { terminals, .. } => {
                // The answer means it heard, not that it has gone: it tears its terminals
                // down first. Waited out here, because a person who stops a session and
                // immediately lists them has to be told the truth.
                let deadline = std::time::Instant::now() + STOP_PATIENCE;
                while std::time::Instant::now() < deadline
                    && std::os::unix::net::UnixStream::connect(&socket).is_ok()
                {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                println!(
                    "qoop stopped, {terminals} {} ended. `qoop` brings the tabs back.",
                    if terminals == 1 {
                        "terminal"
                    } else {
                        "terminals"
                    },
                );
                Ok(())
            }
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            other => Err(std::io::Error::other(format!(
                "the daemon answered a stop with {}",
                other.request_id()
            ))),
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_tab_is_said_without_an_s() {
        assert_eq!(super::count(1, "tab"), "1 tab");
        assert_eq!(super::count(0, "tab"), "0 tabs");
        assert_eq!(super::count(3, "agent"), "3 agents");
    }

    /// An older daemon's "`qoop stop`, then `qoop`" never reaches a person or an agent as
    /// advice; every other refusal is said as it is.
    #[test]
    fn an_older_daemon_is_never_restarted_on_its_own_advice() {
        let said = super::worded(&std::io::Error::other(
            "this daemon is older than the client that called it: it does not know the method \
             that was asked for (unknown variant `SetSpaceGoal`). `qoop stop`, then `qoop`, \
             picks up the new daemon; what is open comes back.",
        ));
        assert!(!said.contains("qoop stop"), "{said}");
        assert!(said.contains("older than this command"), "{said}");
        assert_eq!(
            super::worded(&std::io::Error::other("no task #4")),
            "no task #4"
        );
    }

    /// What the update reloads the services with is this binary's path with every link resolved,
    /// taken before it can be replaced: never `current_exe` read afterwards, which on Linux says
    /// `<path> (deleted)`.
    #[test]
    fn the_path_an_update_reloads_with_is_resolved() {
        let own = super::own_path().expect("a path");
        assert_eq!(std::fs::canonicalize(&own).expect("resolves"), own);
        assert!(!own.to_string_lossy().contains("(deleted)"));
    }

    /// An interface whose daemon closed it with no click comes back when a daemon answers on
    /// the socket (`qoop restart` from another terminal), and says the daemon stopped when
    /// none does. Any other ending is left as it is.
    #[test]
    fn a_closed_interface_comes_back_only_when_a_daemon_answers() {
        let dir = tempfile::tempdir().expect("a directory");
        let socket = dir.path().join("qoop.sock");
        let closed = || Err(std::io::Error::other(qoop_tui::run::DaemonClosed));
        let within = std::time::Duration::from_millis(300);
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("binds");
        assert!(super::moved(&closed(), &socket, within));
        assert!(!super::moved(&Ok(()), &socket, within), "a quit came back");
        assert!(
            !super::moved(&Err(std::io::Error::other("other")), &socket, within),
            "another failure came back"
        );
        // Gone, its socket file left behind, as a daemon killed leaves it.
        drop(listener);
        let at = std::time::Instant::now();
        assert!(!super::moved(&closed(), &socket, within));
        assert!(
            at.elapsed() >= within,
            "gave up before the daemon could be back"
        );
        assert!(!super::moved(
            &closed(),
            &dir.path().join("none.sock"),
            within
        ));
        // One that answers a moment later is waited for.
        let late = dir.path().join("late.sock");
        let bind = std::thread::spawn({
            let late = late.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                std::os::unix::net::UnixListener::bind(late).expect("binds")
            }
        });
        assert!(super::moved(&closed(), &late, within));
        drop(bind.join());
    }

    use super::{
        ContextJson, cli_sender_from, explain_row, is_usage, parse_context_args,
        parse_history_args, parse_history_dir_args, parse_prompt_args, prompt_refusal,
        render_context_text, render_typed, render_window_text, resolve_target, take_session,
        trim_rows, truncate_text, usage,
    };
    use qoop_history::{Message, Role, SessionId, Window};
    use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse};
    use qoop_runtime::paths;
    use std::path::PathBuf;

    fn row(terminal: u32, name: Option<&str>, program: Option<&str>) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
            subagents: 0,
            task: None,
            news: false,
            terminal,
            workspace: 1,
            name: name.map(str::to_string),
            program: program.map(str::to_string),
            behind: None,
            state: "idle".into(),
            agent: true,
            generation: None,
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
            discards: None,
        }
    }

    #[test]
    fn a_terminal_number_resolves_before_any_name() {
        let rows = vec![row(1, Some("1"), None), row(2, None, Some("sh"))];
        assert_eq!(resolve_target(&rows, "2").unwrap().terminal, 2);
    }

    #[test]
    fn an_explanation_names_the_authority_and_the_rule() {
        let mut explained = row(2, None, Some("claude"));
        explained.state = "blocked".into();
        explained.source = Some("ScreenHeuristic".into());
        explained.rule = Some("needs_approval".into());
        explained.manifest = Some("claude".into());
        let text = explain_row(&explained);
        assert!(
            text.contains("state=blocked") && text.contains("source=ScreenHeuristic"),
            "the belief and its authority come first: {text}"
        );
        assert!(
            text.contains("rule=needs_approval") && text.contains("manifest=claude"),
            "then what the screen matched: {text}"
        );
    }

    #[test]
    fn an_explanation_says_what_the_screen_is_missing() {
        let mut row = row(3, None, Some("claude"));
        assert!(
            !explain_row(&row).contains("discarded"),
            "a terminal that lost nothing says nothing about it"
        );
        row.discards = Some(qoop_runtime::control::Discards {
            control_strings: 2,
            control_string_bytes: 9000,
            evicted_unobserved: 1,
            images_flooded: 3,
            ..Default::default()
        });
        let text = explain_row(&row);
        assert!(
            text.contains(
                "discarded: control_strings=2 (9000 bytes) images=0 (0 bytes) images_flooded=3 \
                 evicted_unobserved=1"
            ),
            "the discards are named with their counts: {text}"
        );
    }

    #[test]
    fn an_unmatched_screen_is_an_answer_not_a_gap() {
        // No rule since the generation began: the row says so plainly
        // rather than leaving the field out.
        let text = explain_row(&row(1, None, Some("sh")));
        assert!(
            text.contains("source=unobserved") && text.contains("rule=-"),
            "silence reads as unmatched: {text}"
        );
    }

    #[test]
    fn an_exact_name_resolves() {
        let rows = vec![
            row(1, Some("reviewer"), Some("claude")),
            row(2, None, Some("sh")),
        ];
        assert_eq!(resolve_target(&rows, "reviewer").unwrap().terminal, 1);
    }

    #[test]
    fn only_the_default_session_is_taken_and_any_other_is_pointed_at() {
        let base = tempfile::tempdir().expect("a directory");
        std::fs::set_permissions(
            base.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("chmod");
        // SAFETY: no other test in this binary touches this variable, and the override
        // keeps every read inside the tempdir.
        unsafe { std::env::set_var(paths::RUNTIME_DIR_ENV, base.path()) };
        // What an older qoop across ssh sends: taken out, and the command still reads.
        let mut args: Vec<String> = ["qoop", "--session", "default", "relay"]
            .map(String::from)
            .to_vec();
        take_session(&mut args).expect("the default is accepted");
        assert_eq!(args, vec!["qoop".to_string(), "relay".to_string()]);
        let mut args: Vec<String> = ["qoop", "send", "--session=default", "2", "x"]
            .map(String::from)
            .to_vec();
        take_session(&mut args).expect("the default is accepted");
        assert_eq!(args, ["qoop", "send", "2", "x"].map(String::from).to_vec());
        // A session of its own was a space's worth of tabs: refused, with where it went.
        let refused = take_session(
            &mut ["qoop", "--session", "work", "ls"]
                .map(String::from)
                .to_vec(),
        )
        .expect_err("a named session");
        assert!(
            refused.to_string().contains("replaced by spaces"),
            "{refused}"
        );
        // A flag with nothing after it is a mistake, not the default.
        assert!(take_session(&mut ["qoop", "--session"].map(String::from).to_vec()).is_err());
        // Past `--` it is text to send, not a flag.
        let mut sent: Vec<String> = ["qoop", "send", "3", "--", "--session", "play"]
            .map(String::from)
            .into();
        take_session(&mut sent).expect("text past `--` is not a session");
        assert_eq!(sent.len(), 6, "the text is left as it was: {sent:?}");
        // SAFETY: as above.
        unsafe { std::env::remove_var(paths::RUNTIME_DIR_ENV) };
    }

    #[test]
    fn an_exact_program_resolves_when_no_name_matches() {
        let rows = vec![row(1, None, Some("claude")), row(2, None, Some("sh"))];
        assert_eq!(resolve_target(&rows, "claude").unwrap().terminal, 1);
    }

    #[test]
    fn a_duplicated_name_is_ambiguous_rather_than_first_wins() {
        let rows = vec![row(1, Some("agent"), None), row(2, Some("agent"), None)];
        assert!(resolve_target(&rows, "agent").is_err());
    }

    #[test]
    fn an_unknown_target_names_itself() {
        let rows = vec![row(1, None, Some("sh"))];
        let err = resolve_target(&rows, "nobody").unwrap_err();
        assert!(err.contains("nobody"), "the error names the target: {err}");
    }

    #[test]
    fn a_trust_screen_is_refused_by_name_and_a_shell_is_sendable() {
        let mut trust = row(1, None, Some("codex"));
        trust.state = "blocked".into();
        trust.readiness = "trust_required".into();
        let reason = prompt_refusal(&trust).expect("a trust screen refuses");
        assert!(
            reason.contains("trust prompt"),
            "the fix is answering in the terminal: {reason}"
        );
        let mut approval = row(2, None, Some("claude"));
        approval.state = "blocked".into();
        approval.readiness = "ready".into();
        let reason = prompt_refusal(&approval).expect("an approval refuses");
        assert!(
            !reason.contains("trust"),
            "a mid-task approval is blocked, not unready: {reason}"
        );
        let shell = row(3, None, Some("sh"));
        assert_eq!(prompt_refusal(&shell), None);
    }

    /// The inbox sends text under the rules `qoop prompt` has, ported into the daemon: the
    /// same states are refused, and only a working agent is refused by the inbox alone.
    #[test]
    fn the_inbox_refuses_what_qoop_prompt_refuses() {
        let cases = [
            ("idle", "ready"),
            ("done", "ready"),
            ("starting", "booting"),
            ("failed", "ready"),
            ("uncertain", "ready"),
            ("blocked", "ready"),
            ("blocked", "trust_required"),
            ("idle", "trust_required"),
            ("sleeping", "ready"),
        ];
        for (state, readiness) in cases {
            let mut r = row(1, None, Some("claude"));
            r.state = state.into();
            r.readiness = readiness.into();
            assert_eq!(
                qoop_runtime::asks::refusal(state, readiness).is_some(),
                prompt_refusal(&r).is_some(),
                "{state} {readiness}"
            );
        }
        let mut working = row(1, None, Some("claude"));
        working.state = "working".into();
        assert_eq!(
            prompt_refusal(&working),
            None,
            "qoop prompt sends to a working agent"
        );
        assert_eq!(
            qoop_runtime::asks::refusal("working", "ready"),
            Some("working")
        );
    }

    #[test]
    fn a_sleeping_terminal_refuses_a_prompt_by_name() {
        let mut slept = row(4, None, Some("claude"));
        slept.state = "sleeping".into();
        let reason = prompt_refusal(&slept).expect("a slept tab takes no input");
        assert!(
            reason.contains("sleeping"),
            "the refusal names the state: {reason}"
        );
    }

    #[test]
    fn a_sender_claim_needs_both_a_number_and_a_token() {
        let both = |t: &str, k: &str| cli_sender_from(Some(t.to_string()), Some(k.to_string()));
        assert_eq!(both("3", "tok"), (Some(3), Some("tok".to_string())));
        // Every half or mangled claim sends nothing.
        assert_eq!(
            cli_sender_from(Some("3".to_string()), None),
            (None, None),
            "number without token"
        );
        assert_eq!(
            cli_sender_from(None, Some("tok".to_string())),
            (None, None),
            "token without number"
        );
        assert_eq!(
            cli_sender_from(Some("abc".to_string()), Some("tok".to_string())),
            (None, None),
            "unnumbered terminal"
        );
        assert_eq!(
            cli_sender_from(Some("3".to_string()), Some(String::new())),
            (None, None),
            "empty token"
        );
        assert_eq!(cli_sender_from(None, None), (None, None));
    }

    fn history_argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn history_defaults_to_twenty_newest() {
        let opts = parse_history_args(&history_argv(&["qoop", "history", "2"])).unwrap();
        assert_eq!(opts.target, "2");
        assert_eq!(opts.last, 20);
        assert!(opts.role.is_none());
        assert!(!opts.json);
    }

    #[test]
    fn history_parses_last_role_and_json() {
        let opts = parse_history_args(&history_argv(&[
            "qoop", "history", "claude", "--last", "5", "--role", "user", "--json",
        ]))
        .unwrap();
        assert_eq!(opts.last, 5);
        assert_eq!(opts.role, Some(qoop_history::Role::User));
        assert!(opts.json);
    }

    #[test]
    fn history_rejects_zero_over_max_bad_role_and_unknown_flags() {
        assert!(
            parse_history_args(&history_argv(&["qoop", "history", "2", "--last", "0"])).is_err()
        );
        assert!(
            parse_history_args(&history_argv(&["qoop", "history", "2", "--last", "201"])).is_err()
        );
        assert!(parse_history_args(&history_argv(&["qoop", "history", "2", "--last"])).is_err());
        assert!(
            parse_history_args(&history_argv(&["qoop", "history", "2", "--role", "tool"])).is_err()
        );
        assert!(parse_history_args(&history_argv(&["qoop", "history", "2", "--bogus"])).is_err());
        assert!(parse_history_args(&history_argv(&["qoop", "history"])).is_err());
    }

    #[test]
    fn history_of_a_repo_parses_and_prints_one_line_each() {
        let opts = parse_history_dir_args(&history_argv(&[
            "qoop", "history", "--dir", "/r", "--last", "2000", "--json",
        ]))
        .unwrap();
        assert_eq!(opts.dir, PathBuf::from("/r"));
        assert_eq!(opts.last, 2_000);
        assert!(opts.json);
        assert_eq!(
            parse_history_dir_args(&history_argv(&["qoop", "history", "--dir", "/r"]))
                .unwrap()
                .last,
            200
        );
        for bad in [
            &["qoop", "history", "--dir"][..],
            &["qoop", "history", "--last", "5"],
            &["qoop", "history", "--dir", "/r", "--last", "2001"],
            &["qoop", "history", "--dir", "/r", "--role", "user"],
        ] {
            assert!(
                parse_history_dir_args(&history_argv(bad)).is_err(),
                "{bad:?}"
            );
        }
        let line = render_typed(&[qoop_history::Typed {
            ts: 0,
            product: qoop_history::Product::Codex,
            session: "abcd1234".into(),
            text: format!("two\nlines {}", "x".repeat(500)),
        }]);
        assert!(
            line.starts_with("1970-01-01 codex abcd1234: two lines x"),
            "{line}"
        );
        assert!(line.ends_with("…\n") && line.lines().count() == 1, "{line}");
    }

    #[test]
    fn text_truncation_is_marked() {
        assert_eq!(truncate_text("short"), "short");
        let long = "x".repeat(4001);
        let cut = truncate_text(&long);
        assert!(cut.ends_with("[…truncated]"), "the cap is marked");
        assert!(cut.starts_with(&"x".repeat(4000)));
    }

    fn window(messages: Vec<Message>, truncated: bool, skipped_lines: usize) -> Window {
        Window {
            session: SessionId {
                product: qoop_history::Product::Claude,
                id: "sid-1".to_string(),
            },
            messages,
            truncated,
            skipped_lines,
        }
    }

    #[test]
    fn text_render_pins_names_and_footnotes() {
        let rendered = render_window_text(&window(
            vec![Message {
                role: Role::User,
                text: "hi".to_string(),
                ts: None,
            }],
            true,
            2,
        ));
        assert!(
            rendered
                .stdout
                .starts_with("session product=claude id=sid-1\n"),
            "the pin line comes first: {:?}",
            rendered.stdout
        );
        assert!(rendered.stdout.contains("user: hi\n"));
        assert!(
            rendered
                .stderr
                .contains("skipped 2 corrupt transcript lines"),
            "skipped count on stderr: {:?}",
            rendered.stderr
        );
        assert!(
            rendered.stderr.contains("older messages were not scanned"),
            "truncation footnote on stderr: {:?}",
            rendered.stderr
        );
    }

    #[test]
    fn text_render_is_quiet_when_nothing_to_report() {
        let rendered = render_window_text(&window(vec![], false, 0));
        assert!(rendered.stderr.is_empty());
        assert_eq!(rendered.stdout, "session product=claude id=sid-1\n");
    }

    fn context_argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn context_defaults_to_twenty_newest_and_full_screen() {
        let opts = parse_context_args(&context_argv(&["qoop", "context", "2"])).unwrap();
        assert_eq!(opts.target, "2");
        assert_eq!(opts.last, 20);
        assert!(opts.role.is_none());
        assert_eq!(opts.lines, None);
        assert!(!opts.json);
    }

    #[test]
    fn context_parses_last_role_lines_and_json() {
        let opts = parse_context_args(&context_argv(&[
            "qoop", "context", "claude", "--last", "5", "--role", "user", "--lines", "10", "--json",
        ]))
        .unwrap();
        assert_eq!(opts.last, 5);
        assert_eq!(opts.role, Some(qoop_history::Role::User));
        assert_eq!(opts.lines, Some(10));
        assert!(opts.json);
    }

    #[test]
    fn context_rejects_zero_over_max_bad_role_and_unknown_flags() {
        assert!(
            parse_context_args(&context_argv(&["qoop", "context", "2", "--last", "0"])).is_err()
        );
        assert!(
            parse_context_args(&context_argv(&["qoop", "context", "2", "--last", "201"])).is_err()
        );
        assert!(parse_context_args(&context_argv(&["qoop", "context", "2", "--last"])).is_err());
        assert!(
            parse_context_args(&context_argv(&["qoop", "context", "2", "--role", "tool"])).is_err()
        );
        assert!(parse_context_args(&context_argv(&["qoop", "context", "2", "--bogus"])).is_err());
        assert!(parse_context_args(&context_argv(&["qoop", "context"])).is_err());
        // --lines takes a number, like read.
        assert!(parse_context_args(&context_argv(&["qoop", "context", "2", "--lines"])).is_err());
        assert!(
            parse_context_args(&context_argv(&["qoop", "context", "2", "--lines", "x"])).is_err()
        );
    }

    #[test]
    fn context_text_names_writability_screen_and_history() {
        let mut blocked = row(2, None, Some("claude"));
        blocked.state = "blocked".into();
        let text = render_context_text(
            &blocked,
            prompt_refusal(&blocked).as_deref(),
            &["$ cargo test".to_string()],
            Ok(window(
                vec![Message {
                    role: Role::User,
                    text: "do it".to_string(),
                    ts: None,
                }],
                false,
                0,
            )),
        );
        assert!(text.contains("terminal=2"), "{text}");
        assert!(text.contains("state=blocked"), "{text}");
        assert!(text.contains("writable=no"), "{text}");
        assert!(text.contains("--- screen ---"), "{text}");
        assert!(text.contains("$ cargo test"), "{text}");
        assert!(text.contains("--- history ---"), "{text}");
        assert!(text.contains("user: do it"), "{text}");
    }

    #[test]
    fn context_text_keeps_a_history_refusal_as_a_line() {
        let idle = row(1, None, Some("opencode"));
        let text = render_context_text(&idle, None, &[], Err("no session id reported".into()));
        assert!(text.contains("writable=yes"), "{text}");
        assert!(
            text.contains("--- history: no session id reported"),
            "{text}"
        );
    }

    #[test]
    fn context_json_marks_writability_and_refusals() {
        let idle = row(1, None, Some("opencode"));
        let refused: Result<Window, String> = Err("no session id reported".into());
        let value =
            serde_json::to_value(ContextJson::new(&idle, None, &[], refused)).expect("encode");
        assert_eq!(value["terminal"], 1);
        assert_eq!(value["writable"], true);
        assert_eq!(value["history_error"], "no session id reported");
    }

    #[test]
    fn context_json_carries_a_window_and_a_refusal_reason() {
        let mut blocked = row(2, None, Some("claude"));
        blocked.state = "blocked".into();
        let screen = vec!["$ cargo test".to_string()];
        let won = window(
            vec![Message {
                role: Role::Assistant,
                text: "done".to_string(),
                ts: Some(1000),
            }],
            false,
            0,
        );
        let value = serde_json::to_value(ContextJson::new(
            &blocked,
            prompt_refusal(&blocked).as_deref(),
            &screen,
            Ok(won),
        ))
        .expect("encode");
        assert_eq!(value["writable"], false);
        assert!(
            value["refusal"]
                .as_str()
                .is_some_and(|r| r.contains("blocked")),
            "the reason travels with the flag: {value}"
        );
        assert_eq!(value["screen"], serde_json::json!(["$ cargo test"]));
        assert_eq!(value["history"]["messages"][0]["text"], "done");
        assert!(value.get("history_error").is_none());
    }

    #[test]
    fn trim_rows_drops_blank_tails_then_keeps_the_tail() {
        let rows = vec![
            "a".to_string(),
            "b".to_string(),
            "".to_string(),
            "  ".to_string(),
        ];
        assert_eq!(trim_rows(rows.clone(), None), vec!["a", "b"]);
        assert_eq!(trim_rows(rows, Some(1)), vec!["b"]);
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn prompt_flags_come_right_after_the_target_and_text_is_never_eaten() {
        let opts = parse_prompt_args(&argv(&[
            "qoop",
            "prompt",
            "2",
            "--wait",
            "--timeout",
            "30",
            "run",
            "--wait",
            "please",
        ]))
        .unwrap();
        assert!(opts.wait);
        assert_eq!(opts.timeout, Some(std::time::Duration::from_secs(30)));
        assert_eq!(
            opts.text, "run --wait please",
            "flags inside the text are text"
        );
        for quoted in ["--- notes from review", "--resume please"] {
            let text = parse_prompt_args(&argv(&["qoop", "prompt", "2", quoted])).unwrap();
            assert_eq!(text.text, quoted, "a quoted text is text");
        }
        let dashed =
            parse_prompt_args(&argv(&["qoop", "prompt", "2", "--", "--help", "me"])).unwrap();
        assert_eq!(dashed.text, "--help me");
        assert!(!dashed.wait);
        for late in [
            &["qoop", "prompt", "2", "fix the bug", "--wait"][..],
            &["qoop", "prompt", "2", "fix", "--wait", "--timeout", "9"],
        ] {
            let refused = parse_prompt_args(&argv(late));
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|e| e.starts_with("flags go right after the target")),
                "{late:?}: {:?}",
                refused.err()
            );
        }
        let on_purpose =
            parse_prompt_args(&argv(&["qoop", "prompt", "2", "--", "say", "--wait"])).unwrap();
        assert_eq!(on_purpose.text, "say --wait");
        let typo = parse_prompt_args(&argv(&["qoop", "prompt", "2", "--wiat", "go"]));
        assert!(
            typo.as_ref()
                .is_err_and(|e| e.starts_with("unknown flag --wiat")),
            "{:?}",
            typo.err()
        );
        assert!(parse_prompt_args(&argv(&["qoop", "prompt", "2", "--timeout", "5", "x"])).is_err());
        assert!(parse_prompt_args(&argv(&["qoop", "prompt", "2", "--wait"])).is_err());
        // `--generation` both ways, and never mistaken for a misplaced flag.
        let trailing = parse_prompt_args(&argv(&[
            "qoop",
            "prompt",
            "2",
            "--wait",
            "go on",
            "--generation",
            "7",
        ]))
        .unwrap();
        assert_eq!(
            (trailing.generation, trailing.text.as_str()),
            (Some(7), "go on")
        );
        assert!(trailing.wait);
        let leading = parse_prompt_args(&argv(&[
            "qoop",
            "prompt",
            "2",
            "--generation",
            "7",
            "--wait",
            "go on",
        ]))
        .unwrap();
        assert_eq!(
            (leading.generation, leading.text.as_str()),
            (Some(7), "go on")
        );
        assert!(
            parse_prompt_args(&argv(&[
                "qoop",
                "prompt",
                "2",
                "--generation",
                "7",
                "go",
                "--generation",
                "8",
            ]))
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_cut_exchange_is_made_once_more_and_a_refusal_is_not() {
        use std::io::{Error, ErrorKind};
        use std::sync::atomic::{AtomicU32, Ordering};
        let tries = AtomicU32::new(0);
        let answer = super::once_more_if_cut(|| async {
            match tries.fetch_add(1, Ordering::SeqCst) {
                0 => Err(Error::from(ErrorKind::UnexpectedEof)),
                _ => Ok("taken"),
            }
        })
        .await;
        assert_eq!(answer.expect("the second try"), "taken");
        assert_eq!(tries.load(Ordering::SeqCst), 2);
        tries.store(0, Ordering::SeqCst);
        let refused = super::once_more_if_cut(|| async {
            tries.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(Error::other("refused"))
        })
        .await;
        assert!(refused.is_err());
        assert_eq!(tries.load(Ordering::SeqCst), 1, "a refusal was sent again");
        for (kind, again) in [
            (ErrorKind::ConnectionReset, true),
            (ErrorKind::BrokenPipe, true),
            (ErrorKind::ConnectionRefused, false),
            (ErrorKind::NotFound, false),
        ] {
            assert_eq!(super::cut_by_daemon(&Error::from(kind)), again, "{kind:?}");
        }
    }

    #[test]
    fn a_usage_mistake_is_told_apart_from_a_failure() {
        assert!(is_usage(&usage("usage: qoop wait <target>")));
        assert!(!is_usage(&std::io::Error::other("terminal 3 closed")));
        assert_eq!(usage("usage: x").to_string(), "usage: x");
    }

    /// A daemon at the other end of a pipe that reads the question, then does `then`.
    async fn asked(
        patience: std::time::Duration,
        then: impl AsyncFnOnce(&mut tokio::io::DuplexStream),
    ) -> std::io::Result<ControlResponse> {
        use tokio::io::AsyncReadExt;
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let request = ControlRequest::RestartWhenSafe {
            id: "ask-1".into(),
            version: "9.9.9".into(),
            keep_tabs: false,
            quiet: false,
            same_version: false,
        };
        let daemon = async move {
            let mut read = [0u8; 4096];
            let _ = server.read(&mut read).await;
            then(&mut server).await;
        };
        let (answer, ()) = tokio::join!(
            super::ask_over(client, bytes::BytesMut::new(), &request, patience),
            daemon
        );
        answer
    }

    async fn say(server: &mut tokio::io::DuplexStream, response: &ControlResponse) {
        use tokio::io::AsyncWriteExt;
        let body = serde_json::to_vec(response).expect("json");
        for frame in qoop_proto::chunk(
            qoop_proto::Channel::Control,
            0,
            0,
            None,
            bytes::Bytes::from(body),
        ) {
            let mut buf = bytes::BytesMut::new();
            frame.encode(&mut buf).expect("encode");
            server.write_all(&buf).await.expect("write");
        }
    }

    fn failed(id: &str, reason: &str) -> ControlResponse {
        ControlResponse::Failed {
            id: id.into(),
            reason: reason.into(),
        }
    }

    const SOON: std::time::Duration = std::time::Duration::from_millis(300);

    #[tokio::test]
    async fn an_answer_under_the_questions_id_is_returned() {
        let answer = asked(SOON, async |server| {
            say(server, &failed("other", "not mine")).await;
            say(server, &failed("ask-1", "mine")).await;
        })
        .await;
        assert!(matches!(answer, Ok(ControlResponse::Failed { reason, .. }) if reason == "mine"));
    }

    #[tokio::test]
    async fn a_closed_connection_is_an_end_of_file() {
        let answer = asked(SOON, async |_| {}).await;
        assert_eq!(
            answer.expect_err("closed").kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// An old daemon refuses the ask with no id, naming it: that is the answer. A refusal
    /// with no id that names nothing of ours is not, and is waited past.
    #[tokio::test]
    async fn an_old_daemons_refusal_with_no_id_is_the_answer() {
        let answer = asked(SOON, async |server| {
            say(server, &failed("", "unknown variant `RestartWhenSafe`")).await;
        })
        .await;
        assert!(matches!(answer, Ok(ControlResponse::Failed { .. })));
        let answer = asked(SOON, async |server| {
            say(server, &failed("", "something else")).await;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        })
        .await;
        assert!(
            answer
                .expect_err("ignored")
                .to_string()
                .contains("did not answer")
        );
    }

    #[tokio::test]
    async fn a_silent_daemon_is_given_up_on() {
        let answer = asked(SOON, async |_| {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        })
        .await;
        assert!(
            answer
                .expect_err("silent")
                .to_string()
                .contains("did not answer")
        );
    }
}

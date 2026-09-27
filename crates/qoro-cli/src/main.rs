//! `qoro`: attach to a terminal, starting the daemon if there is not one.
//!
//! The promise this milestone exists to keep is the small one:
//!
//! ```text
//! cd my-project
//! qoro
//! ```
//!
//! reaches a real shell, with no account, no wizard and no manifest, and detaching leaves
//! the shell running.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

mod avatar;
mod hooks;
mod remote;
mod skills;
mod supervise;
mod update;
mod worktree;

use qoro_runtime::client::{self, RenderStream, TerminalGuard};
use qoro_runtime::daemon::ClientWire;
use qoro_runtime::{Daemon, lock, paths};

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

fn main() {
    // One sentence and a failing exit, never the `Error: Custom { kind: .. }` dump that
    // returning the error from `main` prints: every refusal below is worded for a person.
    // Exit 2 when the command was called wrong, 1 when it failed.
    if let Err(e) = run() {
        eprintln!("qoro: {e}");
        std::process::exit(if is_usage(&e) { 2 } else { 1 });
    }
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

fn run() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    // Asked before the session is taken out of the arguments: a remote opened with no
    // `--session` opens that machine's default session, not whichever one was last opened
    // here.
    let session_given = args
        .iter()
        .any(|a| a == "--session" || a.starts_with("--session="))
        || std::env::var(paths::SESSION_ENV).is_ok_and(|s| !s.is_empty());
    // Taken out of the arguments before anything reads them, and put into the environment,
    // because the session decides which directory every path in this process resolves
    // against. Held in one place rather than passed down twelve call sites, and inherited
    // by anything this process starts, which is what makes a shell inside a session able
    // to say `qoro send` and reach its own daemon rather than the default one.
    // The session this process was started in, before `--session` replaces it: what
    // tells "my own terminal" apart from the same number in another session.
    let home_session = std::env::var(paths::SESSION_ENV).ok();
    let session = match take_session(&mut args) {
        Ok(name) => name,
        Err(e) => {
            eprintln!("qoro: {e}");
            std::process::exit(2);
        }
    };
    // SAFETY: single threaded, before anything else runs.
    unsafe { std::env::set_var(paths::SESSION_ENV, &session) };
    match args.get(1).map(String::as_str) {
        Some("daemon") => run_daemon(),
        Some("--hello") => {
            // What a remote bootstrap asks for. JSON on one line, because it is read
            // between markers out of whatever a login shell prints around it, and because
            // the thing that decides compatibility is the codec generation and not the
            // version string (ADR-010).
            println!(
                "{}",
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "codec_generation": qoro_proto::CODEC_GENERATION,
                    "platform": std::env::consts::OS,
                })
            );
            Ok(())
        }
        Some("attach") => run_attach(),
        Some("relay") => run_relay(),
        Some("remote") => remote::run(&args, session_given.then_some(session.as_str())),
        Some("--remote") => run_remote(&args, session_given.then_some(session.as_str())),
        Some("send") => run_send(&args),
        Some("read") => run_read(&args),
        Some("prompt") => run_prompt(&args),
        Some("history") => run_history(&args),
        Some("context") => run_context(&args),
        Some("agents") => run_agents(&args),
        Some("wait") => supervise::run_wait(&args, home_session.as_deref(), &session),
        Some("new") => supervise::run_new(&args),
        Some("close") => supervise::run_close(&args, home_session.as_deref(), &session),
        Some("worktree") => worktree::run(&args),
        Some("setup") => run_setup(&args),
        Some("hook-report") => run_hook_report(&args),
        Some("reload-manifests") => run_reload_manifests(),
        Some("hooks") => hooks::run(&args),
        Some("avatar") => avatar::run(&args),
        Some("skills") => skills::run(&args),
        Some("update") => update::run(&args),
        Some("ls") => run_list_sessions(),
        Some("stop") => run_stop(&args, &session),
        Some("--version" | "-V") => {
            println!("qoro {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            print!(
                "qoro, the portable runtime for human and coding-agent workspaces\n\
                 \n\
                 usage:\n\
                 \x20 qoro            attach to a terminal, starting the daemon if needed\n\
                 \x20 qoro attach     attach to one terminal, with no interface around it\n\
                 \x20 qoro daemon     run the daemon in the foreground\n\
                 \x20 qoro reset      restore a terminal left in raw mode by a killed client\n\
                 \x20 qoro relay      pipe stdin and stdout to this machine's daemon\n\
                 \x20 qoro --remote <name|ssh-target>  the interface for another machine's qoro\n\
                 \x20 qoro remote add <name> <ssh-target> [--session S]  show that\n\
                 \x20                 machine's spaces in `qoro`, beside this one's\n\
                 \x20 qoro remote rm <name>  stop showing them\n\
                 \x20 qoro remote ls  the remotes added\n\
                 \x20 qoro setup ...  capture, inspect, trust and run a Setup\n\
                   \x20 qoro hook-report --state <state> [--event user|tool|blocked|done] [--label ...] [--session-id S]  report this terminal's agent state\n\
                  \x20 qoro hook-report --session-id S  report only the agent's conversation id\n\
                  \x20 qoro send <terminal> <text> [--enter]  type into another terminal\n\
                  \x20 qoro send <terminal> --key NAME...  press named keys: enter esc tab\n\
                  \x20                 shift-tab backspace space up down left right home end\n\
                  \x20                 pageup pagedown ctrl-a..ctrl-z\n\
                  \x20 qoro read <terminal> [--lines N]  print another terminal's screen\n\
                  \x20 qoro prompt <target> [--wait] [--timeout SECS] [--] <text>  send text\n\
                  \x20                 plus Enter, unless blocked; --wait returns once the\n\
                  \x20                 agent took the turn and stopped\n\
                  \x20                 send, prompt and close take --generation G: refused\n\
                  \x20                 when the process is no longer the one that was read\n\
                  \x20 qoro wait [target...] [--until STATE,...] [--match TEXT] [--timeout SECS] [--json]\n\
                  \x20                 block until a terminal reaches a state, or TEXT is on\n\
                  \x20                 its live screen; exits 124 on timeout\n\
                  \x20 qoro new [--name N] [--cwd DIR] [--space ID|NAME] [-- command...]  open a\n\
                  \x20                 terminal, print its number, type the command into it\n\
                  \x20 qoro worktree new <branch> [--base REF] [--path DIR] [--space ID|NAME] [--name NAME] [-- CMD...]\n\
                  \x20                 a git worktree beside the repo, with a terminal in it\n\
                  \x20 qoro worktree ls [--json]  every worktree and the terminals inside\n\
                  \x20 qoro worktree rm <branch|path> [--force]  remove one, keeping the\n\
                  \x20                 branch; refused while any process is inside\n\
                  \x20 qoro close <terminal>  end a terminal and what it started\n\
                  \x20 qoro history <target> [--last N] [--role user|assistant] [--json]\n\
                  \x20                 print another agent's recent transcript window,\n\
                  \x20                 local files only; --json prints the full window\n\
                  \x20 qoro context <target> [--last N] [--role user|assistant] [--lines N] [--json]\n\
                  \x20                 understand another agent before writing to it:\n\
                  \x20                 who it is, whether it is writable, its live screen\n\
                  \x20                 and its recent transcript window\n\
                  \x20 qoro agents [--json]  list every terminal and what it is doing\n\
                  \x20 qoro agents explain <target> [--json]  say why one terminal reads\n\
                  \x20                 the way it does: state, authority, matched rule\n\
                 \x20 qoro reload-manifests  re-read agent-detection overrides\n\
                  \x20 qoro hooks ...  install an agent's own hook, so it says what it is\n\
                  \x20                 doing and which conversation it is having\n\
                  \x20 qoro skills ...  teach an agent what qoro is for, so it looks\n\
                  \x20                 before it writes\n\
                  \x20 qoro ls         list the sessions and say which are running\n\
                  \x20 qoro avatar pick  choose a terminal pet from a batch\n\
                  \x20 qoro avatar show  print the picked pet\n\
                 \x20 qoro stop [name]  end a session: its daemon and everything in it\n\
                 \x20 qoro update [--check]  fetch the newest release, and install it when\n\
                 \x20                 nothing is live\n\
                 \x20 qoro --version\n\
                 \n\
                 options:\n\
                 \x20 --session <name>  which session to work in (or QORO_SESSION).\n\
                 \x20                   sessions do not see each other. the flag wins over\n\
                 \x20                   the environment, which wins over the session last\n\
                 \x20                   opened, which wins over the default.\n\
                 \n\
                 detach with ctrl-q. the shell keeps running.\n\
                 stop ends the processes; opening the session again brings the tabs back.\n"
            );
            Ok(())
        }
        Some("reset") => {
            // SIGKILL cannot be caught, so a killed client can leave the outer terminal in
            // raw mode. This is the escape hatch the honest version of that guarantee needs.
            print!(
                "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[?2031l\x1b[?25h\x1b[?1049l"
            );
            std::io::stdout().flush()?;
            Command::new("stty").arg("sane").status().ok();
            Ok(())
        }
        None => run_client(),
        // A typo is named, not answered with a daemon and the full-screen interface.
        Some(other) => Err(usage(format!(
            "unknown command {other}; usage: qoro [command] [--session <name>], see qoro --help"
        ))),
    }
}

fn run_daemon() -> std::io::Result<()> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    // Held across the whole sequence: stale-socket check, unlink, bind, listen, initialise.
    // Two daemons started at once therefore serialise here rather than both concluding the
    // socket is stale and unlinking each other's.
    let lock = lock::StartupLock::acquire(&paths::lock_path(&dir))?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let daemon = match Daemon::bind(&dir, &shell()).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Another daemon won the race and is already serving. This is the expected
                // outcome for the loser, not a failure.
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        eprintln!("qorod: listening on {}", daemon.socket().display());
        drop(lock);
        daemon.serve().await
    })
}

/// Start the daemon if there is not one, then attach.
fn run_client() -> std::io::Result<()> {
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

    // The open that bare `qoro` will come back to. Written after the daemon is
    // up and before the interface starts, so a failed start records nothing
    // and a good one is what the next bare open finds. Best effort: the attach
    // matters, this file does not.
    record_last_session();
    let rt = tokio::runtime::Runtime::new()?;
    // The interface, not the bare attach. `qoro attach` is still there for the one terminal
    // case and for anything driving this program from a script, because a full screen
    // interface is the wrong thing to hand a pipe.
    // An agent on this machine with no hook installed is worth one line, once. It is not
    // an error and nothing waits on it: an agent is read from its screen either way, and a
    // hook makes that reading immediate and tells qoro which conversation the tab is
    // having, which is what a restart needs.
    let notice = match hooks::missing().as_slice() {
        [] => None,
        agents => Some(format!(
            "{} here without a qoro hook: `qoro hooks install` reads them better",
            agents.join(", ")
        )),
    };
    // Always through the mux, remotes or not: a machine connected from the `+` has to have
    // somewhere to go without restarting the interface. With none added it relays this
    // machine's renders as they come, undecoded.
    let (remotes, notice) = match qoro_remote::saved::load() {
        Ok(remotes) => (remotes, notice),
        Err(e) => {
            let broken = format!("remotes not loaded: {e}");
            let notice = match notice {
                Some(hooks) => format!("{broken}; {hooks}"),
                None => broken,
            };
            (Vec::new(), Some(notice))
        }
    };
    rt.block_on(qoro_tui::mux::run(Some(&socket), remotes, notice))
}

/// `qoro --remote <name|target>`: the interface for another machine's qoro alone.
///
/// A name added with `qoro remote add` brings its own target and session; anything else is
/// an ssh target, opened in the session given with `--session`, else that machine's
/// default. It goes through the same connection the interface keeps for added remotes, so
/// a dropped connection comes back on its own rather than ending the interface.
fn run_remote(args: &[String], session: Option<&str>) -> std::io::Result<()> {
    let Some(what) = args.get(2) else {
        eprintln!("usage: qoro --remote <name|ssh-target> [--session S]");
        std::process::exit(2);
    };
    let added = qoro_remote::saved::load()?
        .into_iter()
        .find(|r| &r.name == what);
    let remote = match added {
        Some(mut remote) => {
            if let Some(session) = session {
                remote.session = session.to_string();
            }
            remote
        }
        None if qoro_remote::saved::valid_target(what) => qoro_remote::Remote {
            name: what.clone(),
            target: what.clone(),
            session: session.unwrap_or("default").to_string(),
        },
        None => {
            eprintln!("qoro: {what:?} is neither an added remote nor an ssh target");
            std::process::exit(2);
        }
    };
    // In the foreground first, where ssh can ask whatever it needs to, and where a remote
    // without qoro says so before the screen is taken over.
    if let Err(e) = remote::check(&remote) {
        eprintln!("qoro: {e}");
        std::process::exit(1);
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(qoro_tui::mux::run(None, vec![remote], None))
}

/// Attach to one terminal, with no interface around it.
fn run_attach() -> std::io::Result<()> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    ensure_daemon(&socket)?;
    record_last_session();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(attach(&socket))
}

/// Remember the session that was just opened, so the next bare `qoro` comes
/// back to it. Best effort by design: a file that cannot be written is a file
/// that is read as absent, and the open still succeeds.
fn record_last_session() {
    let Ok(session) = std::env::var(paths::SESSION_ENV) else {
        return;
    };
    let Ok(base) = paths::runtime_base() else {
        return;
    };
    paths::write_last_session(&base, &session);
}

async fn attach(socket: &std::path::Path) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (stream, leftover) = client::connect(socket, "qoro-cli").await?;
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

/// `qoro setup`: capture, inspect, trust and run.
///
/// Trust is a separate verb on purpose. A Setup that arrives from another machine is
/// executable input (ADR-015), and the person who runs it has to have seen what it will do.
fn run_setup(args: &[String]) -> std::io::Result<()> {
    use qoro_setup::{ConfirmationLedger, level_of};

    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let ledger_path = dir.join("confirmed-setups");

    match args.get(2).map(String::as_str) {
        Some("show") => {
            let path = args
                .get(3)
                .ok_or_else(|| usage("usage: qoro setup show <file>"))?;
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
                .ok_or_else(|| usage("usage: qoro setup trust <file>"))?;
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
            let mut ledger = ConfirmationLedger::open(&ledger_path);
            ledger
                .confirm(&setup.content_hash())
                .map_err(std::io::Error::other)?;
            println!("confirmed {} as {}", setup.name, setup.content_hash());
            Ok(())
        }
        Some("check") => {
            let path = args
                .get(3)
                .ok_or_else(|| usage("usage: qoro setup check <file>"))?;
            let setup = read_setup(path)?;
            let ledger = ConfirmationLedger::open(&ledger_path);
            match qoro_setup::ledger::admit(&setup, &ledger, true) {
                Ok(()) => {
                    println!("confirmed");
                    Ok(())
                }
                // Said as a sentence, like every other refusal, not as the type's dump.
                Err(qoro_setup::ledger::Refusal::NeedsConfirmation { prompt }) => Err(
                    std::io::Error::other(format!("not confirmed yet:\n{prompt}")),
                ),
                Err(qoro_setup::ledger::Refusal::NeedsConfirmationNoClient { name }) => {
                    Err(std::io::Error::other(format!(
                        "{name} is not confirmed, and nobody is here to ask"
                    )))
                }
            }
        }
        _ => {
            print!(
                "usage:\n\
                 \x20 qoro setup show <file>    what it will run, its hash and its level\n\
                 \x20 qoro setup trust <file>   confirm it on this machine\n\
                 \x20 qoro setup check <file>   whether it may run here\n"
            );
            Ok(())
        }
    }
}

fn read_setup(path: &str) -> std::io::Result<qoro_setup::Setup> {
    let text = std::fs::read_to_string(path)?;
    qoro_setup::Setup::from_toml(&text).map_err(|e| std::io::Error::other(e.to_string()))
}

/// `qoro hook-report`: what agent hook scripts call.
///
/// Reads the terminal and token the daemon put in the environment, posts one
/// `ReportAgentState` request, and waits for the answer. Quiet on success: a
/// hook that reports every turn must not spam the transcript. Failures go to
/// stderr with a non-zero exit, and hook scripts swallow them on purpose: the
/// screen reading keeps working with no hooks installed.
fn run_hook_report(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let mut state: Option<String> = None;
    let mut terminal: Option<u32> = std::env::var("QORO_TERMINAL")
        .ok()
        .and_then(|v| v.parse().ok());
    let mut token: Option<String> = std::env::var("QORO_TOKEN").ok();
    let mut visible_blocker = false;
    let mut visible_idle = false;
    let mut visible_working = false;
    let mut id: Option<String> = None;
    let mut session: Option<String> = None;
    let mut event: Option<String> = None;
    let mut label: Option<String> = None;
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--state" => {
                state = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            "--event" => {
                event = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            "--label" => {
                label = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
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
                        "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
                    ));
                }
            }
            "--token" => {
                token = match rest.next() {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(usage(
                            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
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
                            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
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
                            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
                        ));
                    }
                }
            }
            other => {
                return Err(usage(format!(
                    "unknown flag {other}; usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]"
                )));
            }
        }
    }
    let Some(terminal) = terminal else {
        return Err(std::io::Error::other(
            "no terminal: pass --terminal or run inside a qoro terminal (QORO_TERMINAL)",
        ));
    };
    let Some(token) = token else {
        return Err(std::io::Error::other(
            "no token: pass --token or run inside a qoro terminal (QORO_TOKEN)",
        ));
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Session identity without a state claim: a third-party agent that knows
    // its conversation but has no lifecycle event to map. State and session
    // travel separately because saying which conversation this is says
    // nothing about what the agent is doing.
    let request = match state {
        None => {
            if event.is_some() || label.is_some() {
                return Err(usage(
                    "usage: --event/--label need --state; or qoro hook-report --session-id S [--terminal N] [--token T]",
                ));
            }
            let Some(session) = session.filter(|s| !s.trim().is_empty()) else {
                return Err(usage(
                    "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--event user|tool|blocked|done] [--label ...] [--terminal N] [--token T] [--session-id S] or qoro hook-report --session-id S [--terminal N] [--token T]",
                ));
            };
            let id = id.unwrap_or_else(|| format!("hook-session-{terminal}-{nanos}"));
            ControlRequest::ReportAgentSession {
                id,
                terminal,
                token,
                session,
            }
        }
        Some(state) => {
            if qoro_runtime::control::parse_reported_state(&state).is_none() {
                return Err(std::io::Error::other(format!(
                    "unknown agent state {state:?}"
                )));
            }
            if let Some(ref kind) = event
                && !qoro_runtime::control::parse_focus_event(kind)
            {
                return Err(usage(format!(
                    "unknown focus event {kind:?}; usage: qoro hook-report --state <state> [--event user|tool|blocked|done] [--label ...]"
                )));
            }
            if label.is_some() && event.is_none() {
                return Err(usage("usage: --label needs --event user|tool|blocked|done"));
            }
            let id = id.unwrap_or_else(|| format!("hook-{terminal}-{nanos}"));
            ControlRequest::ReportAgentState {
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

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // One bound over the whole exchange, connect and handshake included, not only
        // the answer: a daemon that accepts and never greets would otherwise hold the
        // agent's turn until the agent's own hook timeout, minutes for most of them.
        let answer = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
            let socket = paths::socket_path(&dir);
            let (stream, mut inbuf) = client::connect(&socket, "qoro-hook").await?;
            let (mut rd, mut wr) = stream.into_split();
            let mut seq = 0u32;
            wr.write_all(&client::encode_control_seq(&request, &mut seq)?)
                .await?;
            let mut connection = client::Connection::new();
            let mut read = [0u8; 64 * 1024];
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
        .map_err(|_| std::io::Error::other("the daemon did not answer within 5s"))??;
        match answer {
            ControlResponse::AgentStateReported { .. } => Ok(()),
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

/// `qoro reload-manifests`: re-read agent-detection overrides without restarting.
///
/// Prints what loaded and what did not. A bad file keeps the previous rules,
/// so this is safe to run blindly after editing the override directory.
fn run_reload_manifests() -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoro-cli").await?;
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
    request: &qoro_runtime::control::ControlRequest,
) -> std::io::Result<qoro_runtime::control::ControlResponse> {
    use qoro_runtime::control::ControlResponse;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let id = request.request_id().to_string();
    let (stream, mut inbuf) = client::connect(socket, client_id).await?;
    let (mut rd, mut wr) = stream.into_split();
    let mut seq = 0u32;
    wr.write_all(&client::encode_control_seq(request, &mut seq)?)
        .await?;
    let mut connection = client::Connection::new();
    let mut read = [0u8; 64 * 1024];
    tokio::time::timeout(ASK_PATIENCE, async {
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
    .map_err(|_| {
        std::io::Error::other(format!("the daemon did not answer within {ASK_PATIENCE:?}"))
    })?
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

/// `qoro relay`: the far end of a remote attach.
///
/// Started over ssh, it connects to this machine's daemon socket and copies bytes between
/// that socket and its own stdin and stdout. The protocol does not change: a remote client
/// speaks exactly what a local one speaks, and the transport is a pipe rather than a
/// socket.
///
/// It starts the daemon if there is not one, for the same reason `qoro` does: a person
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
    let _ = up.join();
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
    request: &qoro_runtime::control::ControlRequest,
    timeout: Duration,
) -> std::io::Result<(
    qoro_runtime::control::ControlResponse,
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
                return Err::<qoro_runtime::control::ControlResponse, std::io::Error>(
                    std::io::Error::other("the daemon closed the connection before answering"),
                );
            }
            inbuf.extend_from_slice(&read[..n]);
            for incoming in connection.drain(inbuf)? {
                match incoming {
                    client::Incoming::Control(response) if response.request_id() == want => {
                        return Ok(response);
                    }
                    other => stashed.push(other),
                }
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("the daemon did not answer in time"))??;
    Ok((answer, stashed))
}

/// `qoro send`: type into another terminal's session.
///
/// This travels on the terminal channel's addressed input, the same lane every
/// keystroke uses, and adds no control method that takes bytes for a child.
/// That is why G7.1 keeps passing unchanged: the control codec still has no
/// write method. Same-user processes could already reach any terminal this
/// way; this only gives that path a name plus an existence check.
fn run_send(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};
    use tokio::io::AsyncWriteExt;

    let help = "usage: qoro send <terminal> <text...> [--enter] [--generation G]\n       qoro send <terminal> --key NAME [--key NAME...] [--generation G]";
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
    if !keys.is_empty() {
        supervise::send_keys(terminal, &keys)?;
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
        let (stream, mut inbuf) = client::connect(&socket, "qoro-send").await?;
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
        println!("sent to terminal {terminal}");
        Ok(())
    })
}

/// Who this CLI call runs as, when it runs inside a qoro terminal: the
/// terminal number plus its token, for binding a send/prompt's
/// `from_terminal` claim to its sender. Both come from the environment the
/// daemon put around everything the session starts; a half claim (number
/// without token) sends nothing, and outside a terminal both are absent
/// and the daemon treats the input as unattributed, exactly like before.
fn cli_sender() -> (Option<u32>, Option<String>) {
    cli_sender_from(
        std::env::var("QORO_TERMINAL").ok(),
        std::env::var("QORO_TOKEN").ok(),
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
) -> std::io::Result<qoro_runtime::daemon::SnapshotWire> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};
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
        let mut latest: Option<qoro_runtime::daemon::SnapshotWire> = None;
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

/// `qoro read`: print another terminal's current screen.
///
/// Subscribes, takes the first snapshot for that terminal and prints it.
/// Trailing blank rows are trimmed; `--lines N` keeps the last N rows.
fn run_read(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoro read <terminal> [--lines N]";
    let terminal: u32 = args
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| usage(help))?;
    let mut lines: Option<usize> = None;
    let mut rest = args.iter().skip(3);
    while let Some(arg) = rest.next() {
        if arg == "--lines" {
            lines = Some(
                rest.next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| usage(help))?,
            );
        } else {
            return Err(usage(format!("unknown flag {arg}; {help}")));
        }
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let snapshot = fetch_snapshot(terminal, "qoro-read").await?;
        for row in &trim_rows(snapshot.lines, lines) {
            println!("{row}");
        }
        Ok(())
    })
}

/// Resolve a prompt target against the live index: a terminal number first,
/// then an exact terminal name, then an exact program name.
fn resolve_target<'a>(
    rows: &'a [qoro_runtime::control::AgentRow],
    target: &str,
) -> Result<&'a qoro_runtime::control::AgentRow, String> {
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
fn prompt_refusal(row: &qoro_runtime::control::AgentRow) -> Option<String> {
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

/// `qoro prompt`: send text plus Enter to another session, unless it is blocked.
///
/// Not limited to agents: a
/// shell session accepts a prompt too, it just runs it as a command. The
/// blocked refusal stays, because typing over an interactive question is how a
/// "yes" lands on a question that was never asked. Text and Enter travel as
/// two writes with a short pause between them, so a bracketed paste lands
/// before the newline submits it.
///
/// `--wait` then waits for the turn the prompt caused (`supervise::Turn`), with the
/// exit status of `qoro wait`.
fn run_prompt(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};
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
        let (stream, mut inbuf) = client::connect(&socket, "qoro-prompt").await?;
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
        if let Some(reason) =
            generation.and_then(|g| supervise::generation_refusal(&rows, row.terminal, g))
        {
            return Err(std::io::Error::other(reason));
        }
        let terminal = row.terminal;
        let mut term_seq = 0u32;
        // Both frames carry the sender: either one records the wait edge,
        // and the daemon dedupes by waiter.
        let (from_terminal, from_token) = cli_sender();
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes: text.into_bytes(),
                from_terminal,
                from_token: from_token.clone(),
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
        println!("prompted terminal {terminal}");
        if !wait {
            return Ok(());
        }
        let before = row.clone();
        supervise::finish_turn(supervise::wait_for_turn(&before, timeout).await?)
    })
}

/// Parsed `qoro prompt` arguments, pure so the refusals are testable.
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
    let help = "usage: qoro prompt <target> [--wait] [--timeout SECS] [--generation G] [--] <text...> [--generation G]";
    let mut args = args.to_vec();
    let mut generation = supervise::take_generation(&mut args);
    let target = args.get(2).cloned().ok_or_else(|| help.to_string())?;
    let mut wait = false;
    let mut timeout: Option<Duration> = None;
    let mut literal = false;
    let mut rest = args.iter().skip(3).peekable();
    while let Some(arg) = rest.next_if(|a| a.starts_with("--")) {
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
            "flags go right after the target: qoro prompt <target> --wait <text>; {help}"
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

/// `qoro history`: print another agent's recent transcript window.
///
/// Resolution reuses `resolve_target` and the single `ListAgents` round
/// trip, like `prompt`. The transcript itself is read from the agent
/// product's own store by `qoro-history`, never through the daemon: local
/// files only, newest last. Every refusal names the product and the id (or
/// the missing hook report), never a filesystem path.
fn run_history(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let opts = parse_history_args(args).map_err(usage)?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoro-history").await?;
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
        let row = resolve_target(&agents, &opts.target).map_err(std::io::Error::other)?;
        let program = row.program.clone().unwrap_or_default();
        let base = qoro_history::BaseDirs::from_home()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let window = qoro_history::history(
            &program,
            row.agent_session.as_deref(),
            &base,
            opts.last,
            opts.role,
        )
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

/// Parsed `qoro history` arguments, pure so the refusals are testable.
struct HistoryOpts {
    target: String,
    last: usize,
    role: Option<qoro_history::Role>,
    json: bool,
}

/// History reads at most this many messages per call.
const HISTORY_LAST_MAX: usize = 200;

fn parse_history_args(args: &[String]) -> Result<HistoryOpts, String> {
    let usage = "usage: qoro history <target> [--last N] [--role user|assistant] [--json]";
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
                "user" => qoro_history::Role::User,
                "assistant" => qoro_history::Role::Assistant,
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

fn render_window_text(window: &qoro_history::Window) -> TextRender {
    // The pin travels in --json inside the schema; text mode names it up
    // front so the output is attributable without it.
    let mut stdout = format!(
        "session product={} id={}\n",
        window.session.product.as_str(),
        window.session.id
    );
    for m in &window.messages {
        let role = match m.role {
            qoro_history::Role::User => "user",
            qoro_history::Role::Assistant => "assistant",
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

/// `qoro context`: understand another agent before writing to it.
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
    use qoro_runtime::control::{AgentRow, ControlRequest, ControlResponse};

    let opts = parse_context_args(args).map_err(usage)?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoro-context").await?;
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
            fetch_snapshot(row.terminal, "qoro-context").await?.lines,
            opts.lines,
        );
        let program = row.program.clone().unwrap_or_default();
        let history: Result<qoro_history::Window, String> = qoro_history::BaseDirs::from_home()
            .map_err(|e| e.to_string())
            .and_then(|base| {
                qoro_history::history(
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

/// Parsed `qoro context` arguments, pure so the refusals are testable.
struct ContextOpts {
    target: String,
    last: usize,
    role: Option<qoro_history::Role>,
    lines: Option<usize>,
    json: bool,
}

fn parse_context_args(args: &[String]) -> Result<ContextOpts, String> {
    let usage =
        "usage: qoro context <target> [--last N] [--role user|assistant] [--lines N] [--json]";
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
                "user" => qoro_history::Role::User,
                "assistant" => qoro_history::Role::Assistant,
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
    Window { history: qoro_history::Window },
    Refused { history_error: String },
}

impl<'a> ContextJson<'a> {
    fn new(
        row: &'a qoro_runtime::control::AgentRow,
        refusal: Option<&'a str>,
        screen: &'a [String],
        history: Result<qoro_history::Window, String>,
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
    row: &qoro_runtime::control::AgentRow,
    refusal: Option<&str>,
    screen: &[String],
    history: Result<qoro_history::Window, String>,
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

/// `qoro agents`: the live index, one row per terminal.
///
/// Read only: one `ListAgents` round trip, no subscription, no input. The
/// text form is one `key=value` row per terminal for a person skimming; the
/// `--json` form is the same rows as an array for an agent polling. Both
/// come from the same response, so they cannot disagree.
fn run_agents(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let help = "usage: qoro agents [--json]\n       qoro agents explain <target> [--json]";
    enum Mode<'a> {
        List { json: bool },
        Explain { target: &'a str, json: bool },
    }
    let mut mode = Mode::List { json: false };
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
            } else {
                return Err(usage(format!("unknown flag {arg}; {help}")));
            }
        }
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoro-agents").await?;
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

/// One terminal on one line, for a person skimming `qoro agents`.
///
/// `key=value` rather than columns, because a chosen name can hold a space
/// and columns would then stop lining up exactly when they are needed.
fn agent_line(row: &qoro_runtime::control::AgentRow) -> String {
    let program = row.program.as_deref().unwrap_or("-");
    let name = row.name.as_deref().unwrap_or("-");
    let generation = row
        .generation
        .map(|g| g.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "term={} state={} program={} name={} workspace={} generation={}",
        row.terminal, row.state, program, name, row.workspace, generation
    )
}

/// Why one terminal reads the way it does, for `qoro agents explain`.
///
/// The belief first (state plus the source that holds it), then what the
/// screen last matched in this generation. A missing rule is an answer, not
/// a gap: it says the screen matched nothing since the generation began.
fn explain_row(row: &qoro_runtime::control::AgentRow) -> String {
    let program = row.program.as_deref().unwrap_or("-");
    let generation = row
        .generation
        .map(|g| g.to_string())
        .unwrap_or_else(|| "-".to_string());
    let source = row.source.as_deref().unwrap_or("unobserved");
    let rule = row.rule.as_deref().unwrap_or("-");
    let manifest = row.manifest.as_deref().unwrap_or("-");
    format!(
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
    )
}

/// Start the daemon if there is not one, and wait for its socket.
fn ensure_daemon(socket: &std::path::Path) -> std::io::Result<()> {
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .arg("daemon")
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
    Err(std::io::Error::other(format!(
        "the daemon did not start; check {}",
        socket.display()
    )))
}

/// Pull `--session <name>` out of the arguments, wherever it was written.
///
/// Removed rather than left in place, so every command's own argument parsing sees the
/// arguments it expects: `qoro send --session work 2 "text"` has to mean the same as
/// `qoro --session work send 2 "text"`, and neither may leave `send` counting the flag as
/// its terminal number.
///
/// What was asked wins over the environment, which wins over the last session that was
/// opened, which wins over the default. The file is read only when neither the flag nor
/// the environment names one, and a stale or invalid file is the default, never an error.
fn take_session(args: &mut Vec<String>) -> Result<String, paths::PathError> {
    let mut asked = None;
    let mut at = 1;
    while at < args.len() {
        // No short `-s`: the flag is taken from anywhere in argv, text words of `send`
        // and `prompt` included, and `du -s` or `ls -s` there is text, not a session.
        let taken = if args[at] == "--session" {
            // The value that follows, if there is one. A trailing `--session` with nothing
            // after it is a mistake worth naming rather than a request for the default:
            // `session_name` refuses the empty value.
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
        if let Some(value) = taken {
            asked = Some(value);
        }
    }
    if asked.is_some() {
        return paths::session_name(asked.as_deref());
    }
    if std::env::var(paths::SESSION_ENV)
        .ok()
        .is_some_and(|n| !n.is_empty())
    {
        return paths::session_name(None);
    }
    if let Some(last) = paths::read_last_session(&paths::raw_runtime_base())
        && paths::valid_session_name(&last)
    {
        return paths::session_name(Some(&last));
    }
    paths::session_name(None)
}

/// `qoro ls`: what sessions exist, and which of them are running.
///
/// Read off the disk rather than from a daemon, because the question is usually asked when
/// none of them are running. What is running is answered by connecting: a socket file
/// proves a daemon started once, not that one is there now.
fn run_list_sessions() -> std::io::Result<()> {
    let base = paths::runtime_base().map_err(std::io::Error::other)?;
    let names = paths::sessions(&base);
    if names.is_empty() {
        println!("no sessions yet. `qoro` starts the default one.");
        return Ok(());
    }
    let here = std::env::var(paths::SESSION_ENV).unwrap_or_default();
    for name in names {
        let dir = paths::session_dir(&base, &name);
        let running = std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_ok();
        // What it is holding, read from its own database. A session that is not running
        // still has tabs: they are what the next start brings back.
        let waiting = qoro_store::Store::open(&dir.join("qoro.db"))
            .and_then(|s: qoro_store::Store| s.open_terminal_count())
            .unwrap_or(0);
        let tabs = match waiting {
            1 => "1 tab".to_string(),
            n => format!("{n} tabs"),
        };
        println!(
            "{}{name:<20} {:<8} {tabs}",
            if name == here { "* " } else { "  " },
            if running { "running" } else { "stopped" },
        );
    }
    Ok(())
}

/// `qoro stop [name]`: end a session's daemon, and with it everything it was running.
///
/// The tabs are not lost by this and are not meant to be: what was open stays recorded, so
/// opening the session again brings back the same spaces, the same tabs, the same
/// directories, and carries on the conversation of any agent that was in one. What does
/// not survive is the processes, and that is not a choice: a terminal is a pty this daemon
/// owns, and its children get a hangup when it goes.
fn run_stop(args: &[String], current: &str) -> std::io::Result<()> {
    // `qoro stop work` names a session positionally, which is how a person will write it.
    let name = match args.get(2) {
        Some(named) => paths::session_name(Some(named)).map_err(std::io::Error::other)?,
        None => current.to_string(),
    };
    let base = paths::runtime_base().map_err(std::io::Error::other)?;
    let dir = paths::session_dir(&base, &name);
    let socket = paths::socket_path(&dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        println!("session {name} is not running");
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        use qoro_runtime::control::{ControlRequest, ControlResponse};

        let answer = ask_daemon(
            &socket,
            "qoro-stop",
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
                    "session {name} stopped, {terminals} {} ended. `qoro{}` brings the tabs \
                     back.",
                    if terminals == 1 {
                        "terminal"
                    } else {
                        "terminals"
                    },
                    // Bare `qoro` reopens the session last opened, which is this one
                    // when it is stopped from inside itself. A session stopped from
                    // elsewhere still needs its name spelled out.
                    if name == current {
                        String::new()
                    } else {
                        format!(" --session {name}")
                    }
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
    use super::{
        ContextJson, cli_sender_from, explain_row, is_usage, parse_context_args,
        parse_history_args, parse_prompt_args, prompt_refusal, render_context_text,
        render_window_text, resolve_target, take_session, trim_rows, truncate_text, usage,
    };
    use qoro_history::{Message, Role, SessionId, Window};
    use qoro_runtime::control::AgentRow;
    use qoro_runtime::paths;

    fn row(terminal: u32, name: Option<&str>, program: Option<&str>) -> AgentRow {
        AgentRow {
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
    fn session_selection_is_flag_then_env_then_last_opened_then_default() {
        let base = tempfile::tempdir().expect("a directory");
        // SAFETY: no other test in this binary touches these variables, and the
        // override keeps every read and write inside the tempdir.
        unsafe {
            std::env::set_var(paths::RUNTIME_DIR_ENV, base.path());
            std::env::remove_var(paths::SESSION_ENV);
        }
        let bare = || take_session(&mut vec!["qoro".to_string()]).unwrap();
        assert_eq!(bare(), paths::DEFAULT_SESSION);
        // A recorded session with a directory is what a bare open reopens.
        std::fs::create_dir_all(base.path().join("sessions").join("hobby")).unwrap();
        paths::write_last_session(base.path(), "hobby");
        assert_eq!(bare(), "hobby");
        // The environment wins over the file.
        unsafe { std::env::set_var(paths::SESSION_ENV, "work") };
        assert_eq!(bare(), "work");
        // The flag wins over both.
        assert_eq!(
            take_session(&mut vec![
                "qoro".to_string(),
                "--session".to_string(),
                "play".to_string(),
            ])
            .unwrap(),
            "play"
        );
        // A stale file is the default, never an error.
        unsafe { std::env::remove_var(paths::SESSION_ENV) };
        std::fs::write(paths::last_session_path(base.path()), "gone\n").unwrap();
        assert_eq!(bare(), paths::DEFAULT_SESSION);
        unsafe {
            std::env::remove_var(paths::RUNTIME_DIR_ENV);
            std::env::remove_var(paths::SESSION_ENV);
        }
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
        let opts = parse_history_args(&history_argv(&["qoro", "history", "2"])).unwrap();
        assert_eq!(opts.target, "2");
        assert_eq!(opts.last, 20);
        assert!(opts.role.is_none());
        assert!(!opts.json);
    }

    #[test]
    fn history_parses_last_role_and_json() {
        let opts = parse_history_args(&history_argv(&[
            "qoro", "history", "claude", "--last", "5", "--role", "user", "--json",
        ]))
        .unwrap();
        assert_eq!(opts.last, 5);
        assert_eq!(opts.role, Some(qoro_history::Role::User));
        assert!(opts.json);
    }

    #[test]
    fn history_rejects_zero_over_max_bad_role_and_unknown_flags() {
        assert!(
            parse_history_args(&history_argv(&["qoro", "history", "2", "--last", "0"])).is_err()
        );
        assert!(
            parse_history_args(&history_argv(&["qoro", "history", "2", "--last", "201"])).is_err()
        );
        assert!(parse_history_args(&history_argv(&["qoro", "history", "2", "--last"])).is_err());
        assert!(
            parse_history_args(&history_argv(&["qoro", "history", "2", "--role", "tool"])).is_err()
        );
        assert!(parse_history_args(&history_argv(&["qoro", "history", "2", "--bogus"])).is_err());
        assert!(parse_history_args(&history_argv(&["qoro", "history"])).is_err());
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
                product: qoro_history::Product::Claude,
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
        let opts = parse_context_args(&context_argv(&["qoro", "context", "2"])).unwrap();
        assert_eq!(opts.target, "2");
        assert_eq!(opts.last, 20);
        assert!(opts.role.is_none());
        assert_eq!(opts.lines, None);
        assert!(!opts.json);
    }

    #[test]
    fn context_parses_last_role_lines_and_json() {
        let opts = parse_context_args(&context_argv(&[
            "qoro", "context", "claude", "--last", "5", "--role", "user", "--lines", "10", "--json",
        ]))
        .unwrap();
        assert_eq!(opts.last, 5);
        assert_eq!(opts.role, Some(qoro_history::Role::User));
        assert_eq!(opts.lines, Some(10));
        assert!(opts.json);
    }

    #[test]
    fn context_rejects_zero_over_max_bad_role_and_unknown_flags() {
        assert!(
            parse_context_args(&context_argv(&["qoro", "context", "2", "--last", "0"])).is_err()
        );
        assert!(
            parse_context_args(&context_argv(&["qoro", "context", "2", "--last", "201"])).is_err()
        );
        assert!(parse_context_args(&context_argv(&["qoro", "context", "2", "--last"])).is_err());
        assert!(
            parse_context_args(&context_argv(&["qoro", "context", "2", "--role", "tool"])).is_err()
        );
        assert!(parse_context_args(&context_argv(&["qoro", "context", "2", "--bogus"])).is_err());
        assert!(parse_context_args(&context_argv(&["qoro", "context"])).is_err());
        // --lines takes a number, like read.
        assert!(parse_context_args(&context_argv(&["qoro", "context", "2", "--lines"])).is_err());
        assert!(
            parse_context_args(&context_argv(&["qoro", "context", "2", "--lines", "x"])).is_err()
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
            "qoro",
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
        let dashed =
            parse_prompt_args(&argv(&["qoro", "prompt", "2", "--", "--help", "me"])).unwrap();
        assert_eq!(dashed.text, "--help me");
        assert!(!dashed.wait);
        for late in [
            &["qoro", "prompt", "2", "fix the bug", "--wait"][..],
            &["qoro", "prompt", "2", "fix", "--wait", "--timeout", "9"],
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
            parse_prompt_args(&argv(&["qoro", "prompt", "2", "--", "say", "--wait"])).unwrap();
        assert_eq!(on_purpose.text, "say --wait");
        let typo = parse_prompt_args(&argv(&["qoro", "prompt", "2", "--wiat", "go"]));
        assert!(
            typo.as_ref()
                .is_err_and(|e| e.starts_with("unknown flag --wiat")),
            "{:?}",
            typo.err()
        );
        assert!(parse_prompt_args(&argv(&["qoro", "prompt", "2", "--timeout", "5", "x"])).is_err());
        assert!(parse_prompt_args(&argv(&["qoro", "prompt", "2", "--wait"])).is_err());
        // `--generation` both ways, and never mistaken for a misplaced flag.
        let trailing = parse_prompt_args(&argv(&[
            "qoro",
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
            "qoro",
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
                "qoro",
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

    #[test]
    fn a_usage_mistake_is_told_apart_from_a_failure() {
        assert!(is_usage(&usage("usage: qoro wait <target>")));
        assert!(!is_usage(&std::io::Error::other("terminal 3 closed")));
        assert_eq!(usage("usage: x").to_string(), "usage: x");
    }
}

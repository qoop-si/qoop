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

mod hooks;
mod update;

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

fn main() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    // Taken out of the arguments before anything reads them, and put into the environment,
    // because the session decides which directory every path in this process resolves
    // against. Held in one place rather than passed down twelve call sites, and inherited
    // by anything this process starts, which is what makes a shell inside a session able
    // to say `qoro send` and reach its own daemon rather than the default one.
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
        Some("send") => run_send(&args),
        Some("read") => run_read(&args),
        Some("prompt") => run_prompt(&args),
        Some("agents") => run_agents(&args),
        Some("master") => run_master(&args),
        Some("setup") => run_setup(&args),
        Some("hook-report") => run_hook_report(&args),
        Some("reload-manifests") => run_reload_manifests(),
        Some("hooks") => hooks::run(&args),
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
                 \x20 qoro setup ...  capture, inspect, trust and run a Setup\n\
                  \x20 qoro hook-report --state <state>  report this terminal's agent state\n\
                  \x20 qoro send <terminal> <text> [--enter]  type into another terminal\n\
                  \x20 qoro read <terminal> [--lines N]  print another terminal's screen\n\
                  \x20 qoro prompt <target> <text>  send text plus Enter, unless blocked\n\
                  \x20 qoro agents [--json]  list every terminal and what it is doing\n\
                  \x20 qoro agents explain <target> [--json]  say why one terminal reads\n\
                  \x20                 the way it does: state, authority, matched rule\n\
                  \x20 qoro master brief  print the default supervisor instructions\n\
                 \x20 qoro reload-manifests  re-read agent-detection overrides\n\
                 \x20 qoro hooks ...  install an agent's own hook, so it says what it is\n\
                 \x20                 doing and which conversation it is having\n\
                 \x20 qoro ls         list the sessions and say which are running\n\
                 \x20 qoro stop [name]  end a session: its daemon and everything in it\n\
                 \x20 qoro update [--check]  fetch the newest release, and install it when\n\
                 \x20                 nothing is live\n\
                 \x20 qoro --version\n\
                 \n\
                 options:\n\
                 \x20 --session <name>  which session to work in (or QORO_SESSION).\n\
                 \x20                   sessions do not see each other. the default is\n\
                 \x20                   what you get when you say nothing.\n\
                 \n\
                 detach with ctrl-q. the shell keeps running.\n\
                 stop ends the processes; opening the session again brings the tabs back.\n"
            );
            Ok(())
        }
        Some("reset") => {
            // SIGKILL cannot be caught, so a killed client can leave the outer terminal in
            // raw mode. This is the escape hatch the honest version of that guarantee needs.
            print!("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[?1049l");
            std::io::stdout().flush()?;
            Command::new("stty").arg("sane").status().ok();
            Ok(())
        }
        _ => run_client(),
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
    rt.block_on(qoro_tui::run_with_notice(&socket, notice))
}

/// Attach to one terminal, with no interface around it.
fn run_attach() -> std::io::Result<()> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    ensure_daemon(&socket)?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(attach(&socket))
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
                wr.write_all(&client::encode_command_seq(&ClientWire::Input { bytes: chunk }, &mut seq)?).await?;
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
                .ok_or_else(|| std::io::Error::other("usage: qoro setup show <file>"))?;
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
                .ok_or_else(|| std::io::Error::other("usage: qoro setup trust <file>"))?;
            let setup = read_setup(path)?;
            // Shown before it is trusted, in full. The value of the prompt is that it is
            // rare enough to read, so there is no way to trust something without seeing it.
            print!("{}", setup.confirmation_text());
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
                .ok_or_else(|| std::io::Error::other("usage: qoro setup check <file>"))?;
            let setup = read_setup(path)?;
            let ledger = ConfirmationLedger::open(&ledger_path);
            match qoro_setup::ledger::admit(&setup, &ledger, true) {
                Ok(()) => {
                    println!("confirmed");
                    Ok(())
                }
                Err(refusal) => Err(std::io::Error::other(format!("{refusal:?}"))),
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
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--state" => state = rest.next().cloned(),
            "--terminal" => {
                terminal = rest.next().and_then(|v| v.parse().ok());
            }
            "--token" => token = rest.next().cloned(),
            "--visible-blocker" => visible_blocker = true,
            "--visible-idle" => visible_idle = true,
            "--visible-working" => visible_working = true,
            "--id" => id = rest.next().cloned(),
            // The agent's own id for this conversation. Optional: most hooks do not know
            // one, and the ones that do are what makes a restart come back to this
            // conversation rather than to the newest one in the directory.
            "--session-id" => session = rest.next().cloned(),
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown flag {other}; usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--terminal N] [--token T] [--session-id S]"
                )));
            }
        }
    }
    let Some(state) = state else {
        return Err(std::io::Error::other(
            "usage: qoro hook-report --state <starting|idle|working|blocked|done|failed|unknown> [--terminal N] [--token T]",
        ));
    };
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
    if qoro_runtime::control::parse_reported_state(&state).is_none() {
        return Err(std::io::Error::other(format!(
            "unknown agent state {state:?}"
        )));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let id = id.unwrap_or_else(|| format!("hook-{terminal}-{nanos}"));
    let request = ControlRequest::ReportAgentState {
        id: id.clone(),
        terminal,
        token,
        state,
        visible_blocker,
        visible_idle,
        visible_working,
        clock: nanos,
        session,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoro-hook").await?;
        let (mut rd, mut wr) = stream.into_split();
        let mut seq = 0u32;
        wr.write_all(&client::encode_control_seq(&request, &mut seq)?)
            .await?;
        let mut connection = client::Connection::new();
        let mut read = [0u8; 64 * 1024];
        let answer = tokio::time::timeout(std::time::Duration::from_secs(5), async {
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

    let usage = "usage: qoro send <terminal> <text...> [--enter]";
    let terminal: u32 = args
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| std::io::Error::other(usage))?;
    let mut enter = false;
    let mut parts = Vec::new();
    for arg in args.iter().skip(3) {
        if arg == "--enter" {
            enter = true;
        } else if arg.starts_with("--") {
            return Err(std::io::Error::other(format!(
                "unknown flag {arg}; {usage}"
            )));
        } else {
            parts.push(arg.clone());
        }
    }
    let mut text = parts.join(" ");
    if text.is_empty() {
        return Err(std::io::Error::other(format!(
            "refusing to send empty input; {usage}"
        )));
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
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes: text.into_bytes(),
            },
            &mut term_seq,
        )?)
        .await?;
        wr.flush().await?;
        println!("sent to terminal {terminal}");
        Ok(())
    })
}

/// `qoro read`: print another terminal's current screen.
///
/// Subscribes, takes the first snapshot for that terminal and prints it.
/// Trailing blank rows are trimmed; `--lines N` keeps the last N rows.
fn run_read(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let usage = "usage: qoro read <terminal> [--lines N]";
    let terminal: u32 = args
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| std::io::Error::other(usage))?;
    let mut lines: Option<usize> = None;
    let mut rest = args.iter().skip(3);
    while let Some(arg) = rest.next() {
        if arg == "--lines" {
            lines = Some(
                rest.next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| std::io::Error::other(usage))?,
            );
        } else {
            return Err(std::io::Error::other(format!(
                "unknown flag {arg}; {usage}"
            )));
        }
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use tokio::io::AsyncReadExt;

        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, mut inbuf) = client::connect(&socket, "qoro-read").await?;
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
        // printed while nobody watched was never published. The daemon publishes the
        // live screen on subscribe, so settle briefly and keep the last snapshot
        // rather than the first.
        let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
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
        .map_err(|_| {
            std::io::Error::other(format!("no output from terminal {terminal} within 10s"))
        })??;
        let mut rows = snapshot.lines;
        while rows.last().is_some_and(|l| l.trim().is_empty()) {
            rows.pop();
        }
        if let Some(n) = lines
            && rows.len() > n
        {
            rows = rows[rows.len() - n..].to_vec();
        }
        for row in &rows {
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
fn run_prompt(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};
    use tokio::io::AsyncWriteExt;

    let usage = "usage: qoro prompt <target> <text...>";
    let target = args.get(2).ok_or_else(|| std::io::Error::other(usage))?;
    let text = args[3..].join(" ");
    if text.is_empty() {
        return Err(std::io::Error::other(format!(
            "refusing to send an empty prompt; {usage}"
        )));
    }

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
        let row = resolve_target(&rows, target).map_err(std::io::Error::other)?;
        if let Some(reason) = prompt_refusal(row) {
            return Err(std::io::Error::other(reason));
        }
        let terminal = row.terminal;
        let mut term_seq = 0u32;
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes: text.into_bytes(),
            },
            &mut term_seq,
        )?)
        .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        wr.write_all(&client::encode_command_to(
            terminal,
            &ClientWire::Input { bytes: vec![b'\r'] },
            &mut term_seq,
        )?)
        .await?;
        wr.flush().await?;
        println!("prompted terminal {terminal}");
        Ok(())
    })
}

/// `qoro agents`: the live index, one row per terminal.
///
/// Read only: one `ListAgents` round trip, no subscription, no input. The
/// text form is one `key=value` row per terminal for a person skimming; the
/// `--json` form is the same rows as an array for an agent polling. Both
/// come from the same response, so they cannot disagree.
fn run_agents(args: &[String]) -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let usage = "usage: qoro agents [--json]\n       qoro agents explain <target> [--json]";
    enum Mode<'a> {
        List { json: bool },
        Explain { target: &'a str, json: bool },
    }
    let mut mode = Mode::List { json: false };
    let mut rest = args.iter().skip(2);
    if rest.clone().next().is_some_and(|a| a == "explain") {
        rest.next();
        let Some(target) = rest.next() else {
            return Err(std::io::Error::other(usage.to_string()));
        };
        let mut json = false;
        for arg in rest {
            if arg == "--json" {
                json = true;
            } else {
                return Err(std::io::Error::other(format!(
                    "unknown flag {arg}; {usage}"
                )));
            }
        }
        mode = Mode::Explain { target, json };
    } else {
        for arg in rest {
            if arg == "--json" {
                mode = Mode::List { json: true };
            } else {
                return Err(std::io::Error::other(format!(
                    "unknown flag {arg}; {usage}"
                )));
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

/// `qoro master`: the supervisor account.
///
/// v1 has one verb: `brief` prints the default master instructions, the same
/// text the repository ships, so the person and the panel never disagree
/// about what the master was told. Spawning or pinning stays by hand: there
/// is no create-terminal CLI, and adding one to serve setup would put process
/// spawning on the surface G7 keeps narrow.
fn run_master(args: &[String]) -> std::io::Result<()> {
    let usage = "usage: qoro master brief";
    match args.get(2).map(String::as_str) {
        Some("brief") => {
            print!("{}", include_str!("../../../docs/master-brief.md"));
            Ok(())
        }
        _ => Err(std::io::Error::other(usage.to_string())),
    }
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
fn take_session(args: &mut Vec<String>) -> Result<String, paths::PathError> {
    let mut asked = None;
    let mut at = 1;
    while at < args.len() {
        let taken = if args[at] == "--session" || args[at] == "-s" {
            // The value that follows, if there is one. A trailing `--session` with nothing
            // after it is a mistake worth naming rather than a request for the default.
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
    paths::session_name(asked.as_deref())
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
                    if name == paths::DEFAULT_SESSION {
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
    use super::{explain_row, prompt_refusal, resolve_target};
    use qoro_runtime::control::AgentRow;

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
}

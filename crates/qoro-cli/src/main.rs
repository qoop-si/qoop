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
    let args: Vec<String> = std::env::args().collect();
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
        Some("relay") => run_relay(),
        Some("setup") => run_setup(&args),
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
                 \x20 qoro daemon     run the daemon in the foreground\n\
                 \x20 qoro reset      restore a terminal left in raw mode by a killed client\n\
                 \x20 qoro relay      pipe stdin and stdout to this machine's daemon\n\
                 \x20 qoro setup ...  capture, inspect, trust and run a Setup\n\
                 \x20 qoro --version\n\
                 \n\
                 detach with ctrl-q. the shell keeps running.\n"
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

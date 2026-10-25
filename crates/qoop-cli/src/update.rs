//! `qoop update`: replace this binary with the newest release.
//!
//! A subcommand that needs the network, and it says so in `docs/cli-surface.md` (the other
//! reach of a release build is its daemon's daily usage counts, ADR-023). Everything else behaves the same offline; this one fails
//! closed instead: with the network gone it installs nothing, says so, and exits nonzero.
//!
//! The daemon never fetches or installs anything itself (ADR-019), and neither does the
//! interface. This is a separate short lived process: the person runs it, or, in a release
//! build, the daemon starts it every few hours as `qoop update --auto` (see `auto_timer`),
//! which differs in what it will not do: end a tab, close a window, or touch another
//! machine. By hand it never stops a session either:
//! after the install it asks every daemon, this machine's and each saved remote's, to
//! move onto the new binary (at once with the tabs still running when both binaries can
//! hand tabs over, else at the first moment nothing there is working), and returns
//! as soon as they have answered rather than waiting for the moves. Run again with
//! nothing newer to install, it does only that, so it is also "bring everything to this
//! version".

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use bytes::BytesMut;
use qoop_remote::saved::{self, Remote};
use qoop_runtime::control::{ControlRequest, ControlResponse, keeps_promises};
use qoop_runtime::release::{Release, asset_for, check_sha256, download, fetch_latest};
use qoop_runtime::{client, paths};
use qoop_tui::link::{self, Update, plain};

/// How long a daemon is given to answer the ask to move. Past what the daemon itself takes
/// to look at its terminals twice, which a slow disk stretches.
const ASK_TO_MOVE: Duration = Duration::from_secs(30);

/// How long to look for a daemon that closed its connection without answering, to see
/// whether it came back as the new version.
const COME_BACK: Duration = Duration::from_secs(20);

/// How many times another update holding a remote's swap lock is waited out, and for how
/// long each time, before that remote is reported as busy.
const BUSY_TRIES: u32 = 3;
const BUSY_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_secs(3)
};

/// A staged copy of an install that was killed halfway is swept after this long.
const STALE_STAGE: Duration = Duration::from_secs(30 * 60);

/// The lock a swap of the binary holds, a directory beside it, which the script that
/// updates a remote (`link.rs`) takes by the same name.
const SWAP_LOCK: &str = ".qoop-swap.lock";

/// How often the daemon runs `qoop update --auto`, and how long one run may take.
const AUTO_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const AUTO_LIMIT: Duration = Duration::from_secs(10 * 60);

/// How often a running `--auto` is looked at to see whether it has ended.
const AUTO_POLL: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(1)
};

/// `update.log` is moved aside when it grows past this.
const LOG_LIMIT: u64 = 100 * 1024;

/// A line about a machine: `Ok` when all is as it should be, `Err` when it is not.
type Line = Result<String, String>;

/// Whether this process runs under Rosetta.
fn translated() -> bool {
    Command::new("sysctl")
        .args(["-in", "sysctl.proc_translated"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

/// Whether this binary came out of the release script, which is what sets
/// `QOOP_RELEASE_BUILD=1` for its builds. A build made any other way (cargo by hand, a test, a
/// copy someone put on their PATH) never updates itself, whatever starts it.
pub fn release_built() -> bool {
    matches!(option_env!("QOOP_RELEASE_BUILD"), Some("1"))
}

/// The OS and CPU releases are named for. An Intel build running under Rosetta on an Apple
/// silicon Mac: the machine is arm64, and updating to the Intel build again would keep it
/// translated forever.
fn this_platform() -> (&'static str, &'static str) {
    let os = std::env::consts::OS;
    let arch = if os == "macos" && translated() {
        "aarch64"
    } else {
        std::env::consts::ARCH
    };
    (os, arch)
}

/// `qoop update` checks the releases, installs the newest binary and moves every machine
/// onto it; `qoop update --check` only leaves what it saw in the notice file the interface
/// reads. `qoop update --auto` is the daemon's own timer's: out of `--help`.
pub fn run(args: &[String]) -> std::io::Result<()> {
    // Anything else is refused before the network: a mistyped `--check` read as no
    // flag at all would install instead of looking.
    if let Some(other) = args.iter().skip(2).find(|a| {
        !matches!(
            a.as_str(),
            "--check" | "--help" | "-h" | "--auto" | "--mark-reload"
        )
    }) {
        return Err(std::io::Error::other(format!(
            "unknown argument {other}; usage: qoop update [--check]"
        )));
    }
    // What the swap of a remote runs from the new qoop there when it does not reload the
    // services: the reload is left for that machine's own check.
    if args.iter().any(|a| a == "--mark-reload") {
        return mark_reload();
    }
    if args.iter().any(|a| a == "--auto") {
        return if args.len() == 3 {
            run_auto()
        } else {
            Err(std::io::Error::other(
                "usage: qoop update --auto takes no other argument",
            ))
        };
    }
    let check_only = args.iter().any(|a| a == "--check");
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!(
            "usage:\n\
             \x20 qoop update           install the newest release and move every machine onto it\n\
             \x20 qoop update --check   only note what the newest release is\n"
        );
        return Ok(());
    }
    let current = env!("CARGO_PKG_VERSION");
    let (os, arch) = this_platform();
    let Some(asset) = asset_for(os, arch) else {
        return Err(std::io::Error::other(format!(
            "no prebuilt binary for {os}-{arch}; build from source: \
             cargo build --release -p qoop-cli"
        )));
    };
    let latest = fetch_latest(asset)
        .map_err(|detail| std::io::Error::other(format!("cannot reach the releases: {detail}")));
    if check_only {
        let latest = latest?;
        if !paths::is_newer_version(&latest.version, current) {
            clear_notice();
            println!("qoop {current} is the newest release");
            return Ok(());
        }
        if let Err(e) = write_notice(&latest.version) {
            return Err(std::io::Error::other(format!(
                "saw {}, but cannot leave the notice: {e}",
                latest.version
            )));
        }
        println!(
            "update available: {current} -> {}: run `qoop update` to install it",
            latest.version
        );
        return Ok(());
    }
    // What every machine moves to: the version just installed, or this binary's own when
    // nothing newer is out, and also when the release cannot be read, since what is on
    // disk can be moved onto without the network.
    let mut version = current.to_string();
    match &latest {
        Ok(latest) if paths::is_newer_version(&latest.version, current) => {
            match install(latest, current, false)? {
                Installed::Already(on_disk) => version = on_disk,
                _ => version = latest.version.clone(),
            }
        }
        Ok(_) => {
            clear_notice();
            println!("qoop {current} is the newest release");
        }
        Err(_) => {
            eprintln!("qoop: no release could be read; moving every machine onto {current} anyway")
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let this_machine_ok = runtime.block_on(bring_along(&version));
    latest?;
    if !this_machine_ok {
        return Err(std::io::Error::other(
            "this machine's daemon was not moved: see above",
        ));
    }
    Ok(())
}

/// What the daemon's timer runs. Nothing a person is looking at: it installs the newest
/// release when there is one, and asks this machine's daemon, and only this machine's, to
/// move onto the newest qoop on disk, in a way that ends no tab and closes no window. Where
/// that is not possible the daemon waits, and says why (`update.log`, the foot).
fn run_auto() -> std::io::Result<()> {
    if !release_built() {
        return Ok(());
    }
    let current = env!("CARGO_PKG_VERSION");
    let (os, arch) = this_platform();
    let Some(asset) = asset_for(os, arch) else {
        println!("auto: no prebuilt binary for {os}-{arch}");
        return Ok(());
    };
    // Resolved before an install can replace the file: on Linux the running path then reads
    // `<path> (deleted)`, and a reload asked of that path never runs.
    let exe = crate::own_path().ok_or_else(|| std::io::Error::other("cannot find this binary"))?;
    let latest = fetch_latest(asset);
    let version = auto_version(current, &latest, |latest| install(latest, current, true))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(this_machine(&version, true)) {
        Ok(line) => {
            println!("this machine: {line}");
            // Whatever the install left for the notch and the bridge is done now if the
            // move went through and nobody is connected, else a later run does it.
            let alone = runtime.block_on(alone_on(&version));
            if reload_if_marked(&reload_mark(), alone, || reload_services(&exe)) {
                println!("the notch and the bridge are on {version}");
            }
            Ok(())
        }
        Err(line) => {
            println!("this machine: {line}");
            Err(std::io::Error::other(
                "this machine's daemon was not moved: see above",
            ))
        }
    }
}

/// The version `--auto` asks this machine's daemon onto, after installing what the release
/// answer calls for.
///
/// With nothing newer released, or no answer at all, it is the binary's own version: a
/// daemon that did not move last time (a window was open, the ask failed, a move was
/// latched off) is asked again here, and one already on it answers "already". That is what
/// brings a machine along within one tick of whatever held it.
fn auto_version(
    current: &str,
    latest: &Result<Release, String>,
    install: impl FnOnce(&Release) -> std::io::Result<Installed>,
) -> std::io::Result<String> {
    match latest {
        Ok(latest) if paths::is_newer_version(&latest.version, current) => {
            match install(latest)? {
                Installed::Swapped => Ok(latest.version.clone()),
                Installed::Already(on_disk) => Ok(on_disk),
                // Not installed, for a reason that is no failure (an update already
                // running, a directory not ours to write, a release this binary cannot be
                // moved onto): said, and the daemon is still asked about what is on disk.
                Installed::Skipped(why) => {
                    println!("auto: {} not installed: {why}", latest.version);
                    Ok(current.to_string())
                }
            }
        }
        Ok(_) => Ok(current.to_string()),
        Err(detail) => {
            println!("auto: no release could be read: {detail}");
            Ok(current.to_string())
        }
    }
}

/// Ask this machine's daemon, then each saved remote's, to move to `version`, one line each,
/// and say whether this machine's own step went as it should.
///
/// What goes wrong over there is that machine's line and nothing else: an offline laptop in
/// the list would otherwise fail every update, and the interface tries it again by itself
/// the next time it connects.
async fn bring_along(version: &str) -> bool {
    let this = this_machine(version, false).await;
    let ok = this.is_ok();
    let (Ok(line) | Err(line)) = this;
    println!("this machine: {line}");
    let remotes = match saved::load() {
        Ok(remotes) => remotes,
        Err(e) => {
            println!("remotes: not read: {e}");
            return ok;
        }
    };
    for remote in remotes {
        println!("{}: {}", remote.name, remote_line(&remote, version).await);
    }
    ok
}

/// This machine's daemon: connected, handshaken as one that only watches (it is not a window,
/// so it never holds a quiet move back; the welcome carries the version it runs, which
/// `client::connect` throws away), and that version.
async fn hello(socket: &Path) -> std::io::Result<(tokio::net::UnixStream, BytesMut, String)> {
    welcome(socket, "qoop-update")
        .await
        .map(|(stream, inbuf, welcome)| (stream, inbuf, welcome.server_version))
}

/// The same, with the whole welcome: what the daemon can do and which tabs it took over.
async fn welcome(
    socket: &Path,
    name: &str,
) -> std::io::Result<(tokio::net::UnixStream, BytesMut, qoop_proto::ServerWelcome)> {
    let timed_out =
        |what: &str| std::io::Error::new(std::io::ErrorKind::TimedOut, what.to_string());
    let stream = tokio::time::timeout(ASK_TO_MOVE, tokio::net::UnixStream::connect(socket))
        .await
        .map_err(|_| timed_out("the daemon did not accept the connection"))??;
    let (stream, inbuf, welcome) =
        tokio::time::timeout(ASK_TO_MOVE, client::handshake_observer(stream, name))
            .await
            .map_err(|_| timed_out("the daemon did not answer the handshake"))??;
    Ok((stream, inbuf, welcome))
}

/// How long a daemon that took the ask to restart is given to close the connection, which is
/// its exec: past its own first look, which a slow handover can outlast.
const STILL_MOVING: Duration = Duration::from_secs(15);

/// `qoop restart`: this machine's daemon becomes the qoop on disk now, a build of the same
/// version included, with every tab and agent still running in it (the handover). It ends no
/// tab it can avoid: when the move cannot be live it says why and what a stop would interrupt,
/// and moves nothing; a tab the new binary then cannot take over is brought back and named. No
/// network and no remote, unlike `qoop update`.
pub fn run_restart(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        None => {}
        Some("--help" | "-h") => {
            println!(
                "usage: qoop restart   move the daemon onto the qoop on disk now, every tab and agent still running"
            );
            return Ok(());
        }
        Some(other) => {
            return Err(crate::usage(format!(
                "unknown argument {other}; usage: qoop restart"
            )));
        }
    }
    let socket = paths::socket_path(&paths::runtime_dir().map_err(std::io::Error::other)?);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        println!("qoop is not running; the next qoop starts the one on disk");
        return Ok(());
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(restart_here(&socket))
}

async fn restart_here(socket: &Path) -> std::io::Result<()> {
    let before = rows(socket).await?;
    let (pid, boot) = ask_to_move(socket, &before).await?;
    let moved = moved_onto(socket, pid, boot).await?;
    let adopted: Vec<u32> = moved
        .extra
        .get(qoop_runtime::control::ADOPTED)
        .and_then(|ids| serde_json::from_value(ids.clone()).ok())
        .unwrap_or_default();
    let tabs = carried_live(&before, &adopted, &rows(socket).await?);
    let version = plain(&moved.server_version);
    let meanwhile = if tabs.closed.is_empty() {
        String::new()
    } else {
        format!(
            "; tabs {} closed by themselves meanwhile",
            tab_list(&tabs.closed)
        )
    };
    if !tabs.restored.is_empty() {
        return Err(std::io::Error::other(format!(
            "qoop moved onto {version}, but tabs {} were not carried live: they were ended \
             and brought back, agents resumed in their conversations{meanwhile}",
            tab_list(&tabs.restored)
        )));
    }
    println!(
        "qoop restarted live onto {version}: {} carried in the same process, nothing ended{meanwhile}",
        crate::count(tabs.live.len(), "tab")
    );
    Ok(())
}

/// Ask the daemon to move now, and wait for the exec that cuts the connection. Answers the
/// daemon's pid and the mark of its process image, which the move keeps and changes.
async fn ask_to_move(
    socket: &Path,
    before: &[qoop_runtime::control::AgentRow],
) -> std::io::Result<(Option<u32>, Option<serde_json::Value>)> {
    use tokio::io::AsyncReadExt;
    let (mut stream, inbuf, welcomed) = welcome(socket, "qoop-restart").await?;
    if !welcomed
        .capabilities
        .iter()
        .any(|c| c == qoop_runtime::control::SAME_VERSION_RESTART)
    {
        return Err(not_restarted(
            "the running daemon is from before `qoop restart` and cannot move onto a build of its own version",
            before,
        ));
    }
    let pid = qoop_runtime::daemon::peer_pid(&stream);
    let boot = welcomed.extra.get(qoop_runtime::control::BOOT).cloned();
    let request = ControlRequest::RestartWhenSafe {
        id: crate::request_id("restart"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        keep_tabs: true,
        quiet: false,
        same_version: true,
    };
    match crate::ask_over(&mut stream, inbuf, &request, ASK_TO_MOVE).await {
        Err(e) if crate::cut_by_daemon(&e) => Ok((pid, boot)),
        // No look finished within the daemon's own patience: the exec closing the
        // connection is the move.
        Ok(ControlResponse::RestartArmed {
            waiting_on: None, ..
        }) => {
            let mut sink = [0u8; 4096];
            tokio::time::timeout(STILL_MOVING, async {
                while stream.read(&mut sink).await.is_ok_and(|n| n > 0) {}
            })
            .await
            .map(|()| (pid, boot))
            .map_err(|_| {
                std::io::Error::other(
                    "the daemon took the restart but has not moved yet: it may still be \
                     handing the tabs over; nothing was ended, run `qoop restart` again in a \
                     minute, and its log says why",
                )
            })
        }
        Ok(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        })
        | Ok(ControlResponse::Failed { reason: why, .. }) => Err(not_restarted(&why, before)),
        Ok(_) => Err(std::io::Error::other(
            "the daemon gave an answer this qoop does not understand",
        )),
        Err(e) => Err(e),
    }
}

/// The welcome of the daemon that came back after the cut, when it is the one that moved: an
/// exec keeps the pid and starts a new process image, so the same pid under a new mark is the
/// move. Another pid is a daemon that ended and was started again, and the same mark one whose
/// connection was cut without moving. Never starts a daemon.
async fn moved_onto(
    socket: &Path,
    pid: Option<u32>,
    boot: Option<serde_json::Value>,
) -> std::io::Result<qoop_proto::ServerWelcome> {
    let deadline = std::time::Instant::now() + COME_BACK;
    loop {
        match welcome(socket, "qoop-restart").await {
            Ok((stream, _, welcomed)) if qoop_runtime::daemon::peer_pid(&stream) == pid => {
                if welcomed.extra.get(qoop_runtime::control::BOOT).cloned() == boot {
                    return Err(std::io::Error::other(
                        "the daemon closed the connection but did not move; nothing was ended",
                    ));
                }
                return Ok(welcomed);
            }
            Ok(_) => {
                return Err(std::io::Error::other(
                    "the daemon ended instead of moving, and another one serves now",
                ));
            }
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => {
                return Err(std::io::Error::other(format!(
                    "the daemon did not come back after its move: {e}"
                )));
            }
        }
    }
}

/// The awake tabs from before a move, as it left them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Carried {
    /// Taken over running by the daemon that became the new binary.
    live: Vec<u32>,
    /// Gone, with no row after: closed by themselves.
    closed: Vec<u32>,
    /// A row after, but not taken over: ended and brought back.
    restored: Vec<u32>,
}

fn carried_live(
    before: &[qoop_runtime::control::AgentRow],
    adopted: &[u32],
    after: &[qoop_runtime::control::AgentRow],
) -> Carried {
    let mut tabs = Carried::default();
    for row in before.iter().filter(|row| row.slept_at_unix.is_none()) {
        if adopted.contains(&row.terminal) {
            tabs.live.push(row.terminal);
        } else if after.iter().any(|a| a.terminal == row.terminal) {
            tabs.restored.push(row.terminal);
        } else {
            tabs.closed.push(row.terminal);
        }
    }
    tabs
}

fn tab_list(tabs: &[u32]) -> String {
    tabs.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every tab this machine's daemon has, as `ListAgents` answers.
async fn rows(socket: &Path) -> std::io::Result<Vec<qoop_runtime::control::AgentRow>> {
    let request = ControlRequest::ListAgents {
        id: crate::request_id("restart"),
    };
    match crate::ask_daemon(socket, "qoop-restart", &request).await? {
        ControlResponse::Agents { agents, .. } => Ok(agents),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        _ => Err(std::io::Error::other(
            "unexpected answer to the agent listing",
        )),
    }
}

/// Why nothing moved, said loudly: what a `qoop stop` would interrupt instead, and how.
fn not_restarted(why: &str, rows: &[qoop_runtime::control::AgentRow]) -> std::io::Error {
    let busy: Vec<String> = rows
        .iter()
        .filter(|row| row.slept_at_unix.is_none())
        .filter(|row| {
            if row.agent {
                matches!(row.state.as_str(), "working" | "blocked" | "starting")
            } else {
                row.program
                    .as_deref()
                    .is_some_and(|p| !qoop_runtime::idle::is_shell_program(p))
            }
        })
        .map(|row| {
            let name = row
                .name
                .as_deref()
                .or(row.program.as_deref())
                .unwrap_or("shell");
            format!(
                "\n  tab {} {}: {}",
                row.terminal,
                plain(name),
                plain(&row.state)
            )
        })
        .collect();
    // A script a shell runs shows as that shell, so a stop may end more than is named here.
    let interrupted = if busy.is_empty() {
        "\nno agent is working now; a command still running in a tab would end too".to_string()
    } else {
        format!("\na `qoop stop` would interrupt:{}", busy.concat())
    };
    std::io::Error::other(format!(
        "qoop did not restart: {}; every tab is still running{interrupted}\n\
         `qoop stop`, then `qoop`, restarts the old way: tabs end and agents are resumed",
        plain(why)
    ))
}

/// What came of the ask to this machine's own daemon, or that there is none to ask. `auto`
/// is a move nobody asked for by hand: it ends no tab and closes no window.
async fn this_machine(version: &str, auto: bool) -> Line {
    let socket = match paths::runtime_dir() {
        Ok(dir) => paths::socket_path(&dir),
        Err(e) => return Err(format!("not asked to move: {e}")),
    };
    match hello(&socket).await {
        Ok((stream, inbuf, running)) => {
            let again = || async { hello(&socket).await.map(|(_, _, running)| running) };
            move_daemon(stream, inbuf, &running, version, auto, again).await
        }
        // Only nothing listening is "no daemon": a refused permission or a wrong directory
        // is said as it is.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(format!("no daemon running, the next qoop starts {version}"))
        }
        Err(e) => Err(format!("not asked to move: {e}")),
    }
}

/// What came of putting `version` on one remote and asking its daemon onto it. Everything
/// a remote said is plain text first: it would reach this terminal as it was printed.
async fn remote_line(remote: &Remote, version: &str) -> String {
    // Opened as the interface opens one nobody is watching, with a key of its own: a fixed
    // one would make two runs at once end each other's connection.
    let open = || async { link::open(remote, true, &crate::request_id("update")).await };
    remote_line_with(
        version,
        || put_on(&remote.target, version),
        |on_disk| async move {
            match open().await {
                Ok(link) => {
                    let again = || async { open().await.map(|link| link.server_version.clone()) };
                    move_daemon(
                        link.stream,
                        link.leftover,
                        &link.server_version,
                        &on_disk,
                        false,
                        again,
                    )
                    .await
                }
                Err(e) => Err(format!(
                    "{on_disk} is on its disk, but its daemon was not asked to move: {}",
                    plain(&e.to_string())
                )),
            }
        },
    )
    .await
}

/// `remote_line` with the two things it does over ssh handed in: putting the release there
/// (what is on its disk afterwards, and what the swap warned of), and asking its daemon to
/// move onto that.
async fn remote_line_with<P, PF, M, MF>(version: &str, put: P, move_it: M) -> String
where
    P: FnOnce() -> PF,
    PF: std::future::Future<Output = std::io::Result<Update>>,
    M: FnOnce(String) -> MF,
    MF: std::future::Future<Output = Line>,
{
    let (on_disk, warning) = match put().await {
        Ok(Update::OnDisk(on_disk, warning)) => (on_disk, warning),
        Ok(Update::NoRelease) => {
            return format!("no {version} release, left as it is");
        }
        Err(e) => return format!("not updated: {}", plain(&e.to_string())),
    };
    // What the machine says its qoop is reaches this terminal in every line below, so it
    // has to be a version before it is used as one.
    if paths::parse_version(&on_disk).is_none() {
        return "not updated: the machine reported a version this qoop cannot read".to_string();
    }
    let (Ok(line) | Err(line)) = move_it(on_disk).await;
    match warning {
        Some(warning) => format!("{line}\n  but {warning}"),
        None => line,
    }
}

/// `link::update`, waiting out another update that holds the swap lock over there (an open
/// interface pushing at the same moment): when it finishes, this finds the version on disk.
/// The lock's refusal is told by its sentence in `link.rs`; reword them together.
async fn put_on(target: &str, version: &str) -> std::io::Result<Update> {
    put_on_with(
        |target, version| async move { link::update(&target, &version, true).await },
        target,
        version,
    )
    .await
}

/// `put_on` with the update itself handed in.
async fn put_on_with<F, Fut>(update: F, target: &str, version: &str) -> std::io::Result<Update>
where
    F: Fn(String, String) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<Update>>,
{
    retry_busy(|| update(target.to_string(), version.to_string())).await
}

async fn retry_busy<F, Fut>(mut attempt: F) -> std::io::Result<Update>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<Update>>,
{
    let mut busy = 0;
    loop {
        match attempt().await {
            Err(e)
                if busy < BUSY_TRIES && e.to_string().contains("update of qoop is in progress") =>
            {
                busy += 1;
                tokio::time::sleep(BUSY_WAIT).await;
            }
            done => return done,
        }
    }
}

/// Ask the daemon behind `stream`, which runs `running`, to move to `version`, and say what
/// it answered. Never waits for the move itself: this may be running in a tab of that very
/// daemon, which holds the move until it exits. A daemon that closes the connection with no
/// answer is going, or gone wrong, which is told by looking for it again with `again`.
///
/// `auto` asks it to end no tab and to wait for every window to close. A daemon from before
/// those fields would ignore them and move at once, so it is not asked at all.
async fn move_daemon<S, F, Fut>(
    stream: S,
    inbuf: BytesMut,
    running: &str,
    version: &str,
    auto: bool,
    again: F,
) -> Line
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<String>>,
{
    if !paths::is_newer_version(version, running) {
        return Ok(format!("already {}", plain(running)));
    }
    if auto && !keeps_promises(running) {
        return Err(format!(
            "runs qoop {}, which would end tabs and close windows to move; \
             it moves when you run `qoop update`",
            plain(running)
        ));
    }
    let request = ControlRequest::RestartWhenSafe {
        id: crate::request_id("update"),
        version: version.to_string(),
        keep_tabs: auto,
        quiet: auto,
        same_version: false,
    };
    match crate::ask_over(stream, inbuf, &request, ASK_TO_MOVE).await {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            came_back(version, COME_BACK, again).await
        }
        answer => arm_line(answer, version),
    }
}

/// A daemon closed its connection with no answer, which is what one with nothing in the
/// way does on its way to the new binary, and also what a crash or a dropped ssh looks
/// like: look for it again until `within` has passed, and say it is moving only when it
/// answers as `version`. A daemon still answering as the old one may only be on its way
/// out, so that is waited past too.
async fn came_back<F, Fut>(version: &str, within: Duration, again: F) -> Line
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<String>>,
{
    let deadline = std::time::Instant::now() + within;
    loop {
        // One look is bounded by what is left of the budget, not only the wait between
        // them: a connection and a handshake can each take far longer than the budget.
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let looked = tokio::time::timeout(left.max(Duration::from_millis(1)), again())
            .await
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "no answer in time",
                ))
            });
        let seen = match looked {
            Ok(running) if running == version => {
                return Ok(format!("moving to {} now", plain(version)));
            }
            Ok(running) => format!("runs {}, not {}", plain(&running), plain(version)),
            Err(e) => format!("did not come back as {version}: {}", plain(&e.to_string())),
        };
        if std::time::Instant::now() >= deadline {
            return Err(format!("closed the connection and {seen}"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// One daemon's answer to the ask to move, as a line. What it says is plain text first.
fn arm_line(answer: std::io::Result<ControlResponse>, version: &str) -> Line {
    match answer {
        Ok(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => Ok(format!(
            "moves to {} once nothing is working; waiting on {}",
            plain(version),
            plain(&why)
        )),
        Ok(ControlResponse::RestartArmed { .. }) => Ok(format!(
            "moves to {} once nothing is working",
            plain(version)
        )),
        // A daemon from before the ask existed, told by serde's own wording, as the
        // interface tells it.
        Ok(ControlResponse::Failed { reason, .. })
            if qoop_runtime::control::too_old_for(&reason, None) =>
        {
            Err(
                "runs a qoop from before updates moved on their own; the new one takes over \
                 when it next restarts"
                    .to_string(),
            )
        }
        Ok(ControlResponse::Failed { reason, .. }) => {
            Err(format!("keeps its qoop for now: {}", plain(&reason)))
        }
        Ok(_) => Err("gave an answer this qoop does not understand".to_string()),
        Err(e) => Err(format!("not asked to move: {}", plain(&e.to_string()))),
    }
}

/// How an install came out.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    /// The new binary is in place.
    Swapped,
    /// The file already says a version at least as new (another update got there first).
    Already(String),
    /// Not installed, for a reason that is no failure; only an `--auto` run is ever told.
    Skipped(String),
}

/// Fetch the tarball, check it, and swap it over this binary atomically. `auto` is the
/// daemon's timer's run, which refuses more and complains less.
fn install(latest: &Release, current: &str, auto: bool) -> std::io::Result<Installed> {
    let exe = std::env::current_exe()
        .map_err(|e| std::io::Error::other(format!("cannot find this binary: {e}")))?;
    let installed = install_to(latest, current, &exe, auto)?;
    match &installed {
        Installed::Swapped => {}
        Installed::Already(on_disk) => {
            println!("qoop {on_disk} is already on disk");
            return Ok(installed);
        }
        // Not on disk, so the newer release is left where the foot looks for it.
        Installed::Skipped(_) => {
            let _ = write_notice(&latest.version);
            return Ok(installed);
        }
    }
    // The old name stays a link to this binary (see `legacy`).
    if qoop_runtime::legacy::migrating()
        && let Ok(real) = std::fs::canonicalize(&exe)
    {
        let _ = qoop_runtime::legacy::keep_old_name(&real);
    }
    after_swap(&exe, auto, &reload_mark(), reload_services)?;
    Ok(installed)
}

/// What follows a swap, once the lock is gone. The hooks, skills and icon font are files,
/// refreshed always. The notch and the phone's bridge are put back on the new file at once
/// only when a person asked for this: doing it unattended would cut a phone's live stream. The
/// unattended run leaves a mark, and a later one acts on it when nobody is connected.
fn after_swap(
    exe: &Path,
    auto: bool,
    mark: &Path,
    reload: impl FnOnce(&Path) -> bool,
) -> std::io::Result<()> {
    refresh_integrations(exe);
    if auto {
        write_mark(mark)
    } else if reload(exe) {
        // Done by hand, so nothing is left for a later run.
        let _ = std::fs::remove_file(mark);
        Ok(())
    } else {
        // Said by the reload; the next unattended run tries again.
        write_mark(mark)
    }
}

/// Where the mark is kept that the notch and the bridge still run the binary before this one:
/// beside the notice, in the runtime directory, which a reboot clears along with the
/// services it is about.
fn reload_mark() -> PathBuf {
    paths::update_notice_path().with_file_name("reload-pending")
}

fn mark_reload() -> std::io::Result<()> {
    write_mark(&reload_mark())
}

fn write_mark(mark: &Path) -> std::io::Result<()> {
    if let Some(parent) = mark.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(mark, "")
}

/// A running notch is the old build's; launchd starts the new one in its place. The same for
/// a phone's bridge service, which would otherwise keep the old build or, once launchd has
/// pinned the replaced file, never start again. Asked of the new binary, which is the one
/// the service must run from now on. Whether both are back on it.
fn reload_services(exe: &Path) -> bool {
    let notch = crate::notch::restart_if_installed();
    if !notch {
        eprintln!("qoop: the notch was not reloaded; run `qoop update` again or log in again");
    }
    let reload = Command::new(exe)
        .args(["bridge", "reload"])
        .stdout(Stdio::null())
        .status();
    let bridge = reload.is_ok_and(|status| status.success());
    if !bridge {
        eprintln!("qoop: the bridge service was not reloaded; run `qoop bridge reload`");
    }
    notch && bridge
}

/// Act on the mark of a reload left for later, when nobody is connected to be cut off. Said
/// for a test with the reload handed in. The mark goes only when the reload worked: one that
/// failed (launchd refused) is tried again by the next run, not forgotten with the bridge on
/// the old binary. True when the services are on the new binary now.
fn reload_if_marked(mark: &Path, alone: bool, reload: impl FnOnce() -> bool) -> bool {
    if !alone || !mark.exists() || !reload() {
        return false;
    }
    let _ = std::fs::remove_file(mark);
    true
}

/// Whether this machine's daemon runs `version` (or newer), or none runs, and no client is
/// connected to it: a quiet ask answers "already there" with what holds it, if anything. A
/// daemon that does not keep the promises is never asked.
async fn alone_on(version: &str) -> bool {
    let socket = match paths::runtime_dir() {
        Ok(dir) => paths::socket_path(&dir),
        Err(_) => return false,
    };
    match hello(&socket).await {
        Ok((stream, inbuf, running)) => {
            if paths::is_newer_version(version, &running) || !keeps_promises(&running) {
                return false;
            }
            let request = ControlRequest::RestartWhenSafe {
                id: crate::request_id("update"),
                version: version.to_string(),
                keep_tabs: true,
                quiet: true,
                same_version: false,
            };
            matches!(
                crate::ask_over(stream, inbuf, &request, ASK_TO_MOVE).await,
                Ok(ControlResponse::RestartArmed {
                    waiting_on: None,
                    ..
                })
            )
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            true
        }
        Err(_) => false,
    }
}

/// What `scripts/install.sh` does after it puts a binary down: the new build's own `hooks
/// install` and `skills install`, which skip an agent that is not on this machine, so the
/// files agents read are never older than the binary, and `icons install`, so the icon font
/// is in place before the next interface starts. A failure is said and never fails
/// an update that has already happened.
fn refresh_integrations(exe: &Path) {
    // The icon font only where it is drawn from, as `install.sh` does.
    let icons = cfg!(target_os = "macos").then_some("icons");
    for what in ["hooks", "skills"].into_iter().chain(icons) {
        if !Command::new(exe)
            .args([what, "install"])
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!("qoop: the {what} were not refreshed; run `qoop {what} install`");
        }
    }
}

/// The directory a swap holds, as the update script for a remote does (`link.rs`): taken
/// once and never waited for, so two updates cannot deadlock, only one of them gives way.
struct SwapLock(PathBuf);

impl SwapLock {
    /// `None` when another update holds it. A lock left by one that was killed is taken
    /// away after half an hour, as the script does.
    fn take(dir: &Path) -> std::io::Result<Option<Self>> {
        let path = dir.join(SWAP_LOCK);
        let stale = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_STAGE);
        if stale {
            let _ = std::fs::remove_dir(&path);
        }
        match std::fs::create_dir(&path) {
            Ok(()) => Ok(Some(Self(path))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Drop for SwapLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// The version the file at `exe` says it is, if it runs and says one.
fn version_at(exe: &Path) -> Option<String> {
    let out = Command::new(exe).arg("--version").output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout);
    let version = line.trim().split_once(' ')?.1.to_string();
    paths::parse_version(&version).map(|_| version)
}

/// The same swap onto an explicit path, so tests drive the whole chain without
/// touching the test binary they are running inside of.
///
/// Everything slow happens before the swap lock is taken and the lock is let go before
/// anything else runs, so the Mac's push to a remote, which waits a few seconds for it, never
/// finds it held across a reload.
fn install_to(
    latest: &Release,
    current: &str,
    exe: &Path,
    auto: bool,
) -> std::io::Result<Installed> {
    // The file itself, not the link it was launched through: `current_exe` on macOS is
    // the PATH spelling, and renaming over a link turns it into a plain file that no
    // later build of what it pointed at ever reaches again.
    let exe = &std::fs::canonicalize(exe)
        .map_err(|e| std::io::Error::other(format!("cannot resolve {exe:?}: {e}")))?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    sweep_stale_stages(dir);
    // A private directory beside the file, with an unpredictable name and one per install:
    // a name another user of a shared /tmp can guess is one they can fill first, a /tmp
    // that is mounted noexec cannot run what is unpacked there, and a killed install's
    // leftovers are found by the sweep above.
    let work_dir = match tempfile::Builder::new()
        .prefix(".qoop-update.")
        .tempdir_in(dir)
    {
        Ok(work_dir) => work_dir,
        Err(e) if auto => {
            return Ok(Installed::Skipped(format!(
                "{} is not this user's to write: {e}",
                dir.display()
            )));
        }
        Err(e) => {
            return Err(std::io::Error::other(format!(
                "cannot write beside {}: {e}",
                exe.display()
            )));
        }
    };
    let work = work_dir.path();
    let failed = |message: String| std::io::Error::other(message);

    let tarball = work.join(&latest.asset);
    if let Err(detail) = download(&latest.asset_url, &tarball) {
        return Err(failed(format!("the download failed: {detail}")));
    }
    match &latest.checksum_url {
        Some(url) => {
            let checksum_file = work.join(format!("{}.sha256", latest.asset));
            match download(url, &checksum_file) {
                Ok(()) => {
                    if let Err(detail) = check_sha256(&tarball, &checksum_file) {
                        return Err(failed(format!(
                            "refusing to update: the checksum does not match: {detail}"
                        )));
                    }
                }
                // The release says it has a checksum, so failing to fetch it is not the
                // same as there being none: installing unverified here would skip the one
                // integrity check exactly when something went wrong.
                Err(detail) => {
                    return Err(failed(format!(
                        "refusing to update: the release lists a checksum that cannot be \
                         downloaded: {detail}"
                    )));
                }
            }
        }
        // Nobody is there to say yes to an unverified binary.
        None if auto => {
            return Err(failed(
                "refusing to update: the release lists no checksum".to_string(),
            ));
        }
        None => {}
    }
    let tar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .current_dir(work)
        .status()
        .map_err(|e| failed(format!("cannot unpack the release: {e}")))?;
    if !tar.success() {
        return Err(failed(format!(
            "cannot unpack the release: tar exited {tar}"
        )));
    }
    let fresh = work.join("qoop");
    let reported = Command::new(&fresh)
        .arg("--version")
        .output()
        .map_err(|e| failed(format!("the downloaded binary does not run: {e}")))?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("qoop {}", latest.version) {
        return Err(failed(format!(
            "refusing to update: the download reports {reported:?}, want {:?}",
            format!("qoop {}", latest.version)
        )));
    }
    if auto && let Some(generation) = other_codec(&fresh)? {
        return Ok(Installed::Skipped(format!(
            "speaks codec generation {generation}, not {}; run `qoop update` to install it",
            qoop_proto::CODEC_GENERATION
        )));
    }
    let Some(lock) = SwapLock::take(dir)? else {
        return if auto {
            Ok(Installed::Skipped("another update is running".to_string()))
        } else {
            Err(failed(
                "another update of qoop is in progress; try again later".to_string(),
            ))
        };
    };
    // Read again under the lock: another update may have put this release there while this
    // one was downloading it.
    if let Some(on_disk) = version_at(exe)
        && !paths::is_newer_version(&latest.version, &on_disk)
    {
        return Ok(Installed::Already(on_disk));
    }
    // Staged under a name of its own, beside the file so the rename stays on one filesystem:
    // two updates at once each rename a whole verified copy, never one half written.
    let staged = tempfile::Builder::new()
        .prefix(".qoop-update.")
        .tempfile_in(dir)?;
    std::fs::copy(&fresh, staged.path())?;
    std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))?;
    staged.as_file().sync_all()?;
    if let Err(e) = staged.persist(exe) {
        return Err(failed(format!("cannot replace {exe:?}: {}", e.error)));
    }
    drop(lock);
    clear_notice();
    writeln!(
        std::io::stdout(),
        "updated qoop {current} -> {} at {}",
        latest.version,
        exe.display()
    )?;
    Ok(Installed::Swapped)
}

/// The codec generation the binary at `fresh` speaks, when it is not this one's: a daemon
/// cannot be moved onto it live, and nobody is there to be told what that costs.
fn other_codec(fresh: &Path) -> std::io::Result<Option<u64>> {
    let said = Command::new(fresh)
        .arg("--hello")
        .output()
        .map_err(|e| std::io::Error::other(format!("the downloaded binary does not run: {e}")))?;
    let hello: serde_json::Value = serde_json::from_slice(&said.stdout).map_err(|_| {
        std::io::Error::other("refusing to update: the download does not say its codec generation")
    })?;
    let generation = hello["codec_generation"].as_u64().ok_or_else(|| {
        std::io::Error::other("refusing to update: the download does not say its codec generation")
    })?;
    Ok((generation != u64::from(qoop_proto::CODEC_GENERATION)).then_some(generation))
}

/// Take away what a killed install left in `dir`: a staged binary or an unpacked release,
/// a whole binary's worth each, which a fixed staging name used to overwrite the next time.
/// Only old ones, so an install running right now keeps its own.
fn sweep_stale_stages(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(".qoop-update.")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_STAGE);
        if stale {
            let path = entry.path();
            let _ = std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path));
        }
    }
}

/// The file whose presence turns the daemon's own checks off. Looked for at every tick, so
/// it needs no restart and holds for a daemon started from any shell.
fn opt_out_path() -> PathBuf {
    paths::no_auto_update_path()
}

/// Run `exe update --auto` now and then, for as long as this daemon lives: the first time 3
/// to 10 minutes after it starts (machines that start together do not ask together), then
/// every six hours. The daemon only schedules; the child does the network, the install and
/// the ask, and is killed if it takes more than ten minutes. Its output goes to `update.log`.
pub async fn auto_timer(exe: PathBuf) {
    let first = Duration::from_secs(3 * 60)
        + Duration::from_secs(
            std::hash::BuildHasher::hash_one(&std::hash::RandomState::new(), 0u8) % (7 * 60),
        );
    let log = paths::logs_dir().map(|dir| dir.join("update.log"));
    auto_every(
        &exe,
        &opt_out_path(),
        log.as_deref(),
        first,
        AUTO_EVERY,
        AUTO_LIMIT,
    )
    .await;
}

/// `auto_timer` with its places and durations handed in.
async fn auto_every(
    exe: &Path,
    opt_out: &Path,
    log: Option<&Path>,
    first: Duration,
    every: Duration,
    limit: Duration,
) {
    tokio::time::sleep(first).await;
    loop {
        auto_once(exe, opt_out, log, limit).await;
        tokio::time::sleep(every).await;
    }
}

/// One run of `exe update --auto` unless the person turned it off, waited for and reaped,
/// or killed once `limit` has passed.
async fn auto_once(exe: &Path, opt_out: &Path, log: Option<&Path>, limit: Duration) {
    if opt_out.exists() {
        return;
    }
    note(log, "update --auto");
    let (out, err) = log_stdio(log);
    let mut child = match Command::new(exe)
        .args(["update", "--auto"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return note(log, &format!("could not start {}: {e}", exe.display())),
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return note(log, &format!("ended: {status}")),
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return note(log, &format!("killed after {}s", limit.as_secs()));
            }
            Ok(None) => tokio::time::sleep(AUTO_POLL).await,
            Err(e) => return note(log, &format!("lost track of it: {e}")),
        }
    }
}

/// The log opened for appending, after it was moved aside if it has grown past
/// [`LOG_LIMIT`]: it holds the last hundred KB or two and no more. Readable by the person only.
fn open_log(log: &Path) -> Option<std::fs::File> {
    qoop_runtime::say::open_log(log, LOG_LIMIT)
}

/// Where a run's output goes: the log, or nowhere.
fn log_stdio(log: Option<&Path>) -> (Stdio, Stdio) {
    match log.and_then(open_log) {
        Some(file) => match file.try_clone() {
            Ok(twin) => (file.into(), twin.into()),
            Err(_) => (file.into(), Stdio::null()),
        },
        None => (Stdio::null(), Stdio::null()),
    }
}

/// A line in the log with the time, in seconds since 1970.
fn note(log: Option<&Path>, what: &str) {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Some(mut file) = log.and_then(open_log) {
        let _ = writeln!(file, "[{at}] {what}");
    }
}

/// Leave the newest version where the interface looks without touching the network.
fn write_notice(version: &str) -> std::io::Result<()> {
    let path = paths::update_notice_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{version}\n"))
}

/// A notice that no longer points ahead is worse than none.
fn clear_notice() {
    let path = paths::update_notice_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let seen = text.trim().trim_start_matches('v');
        if !paths::is_newer_version(seen, env!("CARGO_PKG_VERSION")) {
            std::fs::remove_file(&path).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::{
        AUTO_LIMIT, BytesMut, ControlResponse, Installed, LOG_LIMIT, Line, Release, SWAP_LOCK,
        Update, after_swap, arm_line, auto_every, auto_once, auto_version, came_back, install_to,
        keeps_promises, move_daemon, note, put_on_with, release_built, reload_if_marked,
        remote_line_with, retry_busy, run,
    };

    fn tab(terminal: u32, asleep: bool) -> qoop_runtime::control::AgentRow {
        serde_json::from_value(serde_json::json!({
            "terminal": terminal,
            "workspace": 1,
            "program": null,
            "behind": null,
            "state": "idle",
            "slept_at_unix": asleep.then_some(1),
        }))
        .expect("a row")
    }

    /// A restart reports every awake tab as carried, closed by itself, or ended and brought
    /// back, and leaves a sleeping tab out.
    #[test]
    fn a_restart_tells_carried_tabs_from_closed_and_restored_ones() {
        let before = [tab(1, false), tab(2, false), tab(3, false), tab(4, true)];
        let after = [tab(1, false), tab(3, false), tab(4, true)];
        assert_eq!(
            super::carried_live(&before, &[1], &after),
            super::Carried {
                live: vec![1],
                closed: vec![2],
                restored: vec![3],
            }
        );
        assert_eq!(
            super::carried_live(&before, &[1, 2, 3], &after),
            super::Carried {
                live: vec![1, 2, 3],
                ..Default::default()
            }
        );
    }

    const MAC_ARM: &str = "qoop-aarch64-apple-darwin.tar.gz";

    /// A stand-in release tarball: a script answering `--version` the way the
    /// real binary does, packed the way the release script packs it.
    fn fake_release(dir: &std::path::Path, reported: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("qoop");
        std::fs::write(&script, format!("#!/bin/sh\necho \"qoop {reported}\"\n")).expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let tarball = dir.join("qoop-aarch64-apple-darwin.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&tarball)
            .arg("qoop")
            .current_dir(dir)
            .status()
            .expect("tar");
        assert!(status.success());
        format!("file://{}", tarball.display())
    }

    #[test]
    fn install_swaps_the_binary_after_verifying_its_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoop");
        std::fs::write(&exe, "old binary").expect("old");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: None,
        };
        install_to(&latest, "0.0.0", &exe, false).expect("install");
        let out = std::process::Command::new(&exe)
            .arg("--version")
            .output()
            .expect("run the swapped binary");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "qoop 9.9.9",
            "the file at the old path is the new release now"
        );
    }

    /// A release that lists a checksum and then cannot serve it is not a release
    /// without one: installing unverified there skips the only integrity check.
    #[test]
    fn a_checksum_that_cannot_be_downloaded_refuses_the_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoop");
        std::fs::write(&exe, "old binary").expect("old");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: Some(format!("file://{}/missing.sha256", pack.display())),
        };
        assert!(install_to(&latest, "0.0.0", &exe, false).is_err());
        assert_eq!(
            std::fs::read(&exe).expect("read"),
            b"old binary",
            "an unverifiable download must not touch the binary in hand"
        );
    }

    /// Launched through a link on PATH, the update lands on the file the link points
    /// at, and the link stays a link: replacing it would cut it off from every later
    /// build of the file it pointed to.
    #[test]
    fn install_through_a_symlink_replaces_the_file_it_points_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let real = dir.path().join("real-qoop");
        std::fs::write(&real, "old binary").expect("old");
        let link = dir.path().join("qoop");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: None,
        };
        install_to(&latest, "0.0.0", &link, false).expect("install");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link was replaced by a plain file"
        );
        let out = std::process::Command::new(&real)
            .arg("--version")
            .output()
            .expect("run the swapped binary");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "qoop 9.9.9");
    }

    #[test]
    fn a_download_reporting_the_wrong_version_leaves_the_binary_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoop");
        std::fs::write(&exe, "old binary").expect("old");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "0.0.0"),
            checksum_url: None,
        };
        assert!(install_to(&latest, "0.0.0", &exe, false).is_err());
        assert_eq!(
            std::fs::read(&exe).expect("read"),
            b"old binary",
            "a refused download must not touch the binary in hand"
        );
    }

    fn armed(waiting_on: Option<&str>) -> std::io::Result<ControlResponse> {
        Ok(ControlResponse::RestartArmed {
            id: "u".into(),
            waiting_on: waiting_on.map(str::to_string),
        })
    }

    fn failed(reason: &str) -> std::io::Result<ControlResponse> {
        Ok(ControlResponse::Failed {
            id: String::new(),
            reason: reason.to_string(),
        })
    }

    /// Each answer to the ask to move reads as one line that says what happens next, and
    /// only a refusal or an answer it cannot read is a failure.
    #[test]
    fn each_answer_to_the_ask_to_move_is_one_line() {
        assert_eq!(
            arm_line(armed(Some("tab 3: sleep is running in it")), "0.7.0"),
            Ok(
                "moves to 0.7.0 once nothing is working; waiting on tab 3: sleep is running in it"
                    .to_string()
            )
        );
        assert_eq!(
            arm_line(armed(None), "0.7.0"),
            Ok("moves to 0.7.0 once nothing is working".to_string())
        );
        assert_eq!(
            arm_line(failed("not started as `qoop daemon`"), "0.7.0"),
            Err("keeps its qoop for now: not started as `qoop daemon`".to_string())
        );
        assert!(
            arm_line(failed("unknown variant `RestartWhenSafe`"), "0.7.0")
                .expect_err("old daemon")
                .starts_with("runs a qoop from before updates moved")
        );
        assert_eq!(
            arm_line(Err(std::io::Error::other("timed out")), "0.7.0"),
            Err("not asked to move: timed out".to_string())
        );
        assert!(
            arm_line(
                Ok(ControlResponse::Stopping {
                    id: "u".into(),
                    terminals: 0,
                }),
                "0.7.0"
            )
            .is_err()
        );
    }

    /// What a remote's daemon says never reaches the terminal as an escape.
    #[test]
    fn what_a_daemon_says_is_shown_plain() {
        let said = arm_line(armed(Some("tab 1\u{1b}[2J: busy")), "0.7.0").expect("armed");
        assert!(!said.contains('\u{1b}'), "{said:?}");
        let said = arm_line(failed("no\u{7}pe"), "0.7.0").expect_err("refused");
        assert!(!said.contains('\u{7}'), "{said:?}");
    }

    /// A closed connection is "moving" only when the daemon comes back saying it is the new
    /// version; anything else says what was seen.
    #[tokio::test]
    async fn a_closed_connection_is_moving_only_when_the_daemon_says_so() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let soon = std::time::Duration::from_millis(600);
        let calls = AtomicU32::new(0);
        // Gone for a moment, then still the old one, then the new one.
        let said = came_back("0.7.0", std::time::Duration::from_secs(20), || async {
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 => Err(std::io::Error::other("connection refused")),
                1 => Ok("0.6.0".to_string()),
                _ => Ok("0.7.0".to_string()),
            }
        })
        .await;
        assert_eq!(said, Ok("moving to 0.7.0 now".to_string()));

        let said = came_back("0.7.0", soon, || async { Ok("0.6.0".to_string()) }).await;
        assert_eq!(
            said,
            Err("closed the connection and runs 0.6.0, not 0.7.0".to_string())
        );
        let said = came_back("0.7.0", soon, || async {
            Err(std::io::Error::other("Connection timed out\u{1b}[2J"))
        })
        .await;
        assert_eq!(
            said,
            Err(
                "closed the connection and did not come back as 0.7.0: Connection timed out[2J"
                    .to_string()
            )
        );
    }

    /// A look at a daemon that takes longer than what is left of the budget is cut off at it:
    /// a connection and a handshake can each take far longer than the budget itself.
    #[tokio::test]
    async fn one_slow_look_does_not_outlast_the_budget() {
        let started = std::time::Instant::now();
        let said = came_back("0.7.0", std::time::Duration::from_millis(300), || async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok("0.7.0".to_string())
        })
        .await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(said.is_err(), "{said:?}");
    }

    /// A remote's own words never reach the terminal as anything but text: what the daemon says
    /// it runs, and what it says is on its disk.
    #[tokio::test]
    async fn what_a_remote_calls_its_version_is_text_or_refused() {
        let (stream, _other) = tokio::io::duplex(64);
        let said = move_daemon(
            stream,
            BytesMut::new(),
            "0.7.0\u{1b}[31m",
            "0.7.0",
            false,
            || async { Ok(String::new()) },
        )
        .await;
        let said = said.expect("not newer, so already there");
        assert!(!said.contains('\u{1b}'), "{said:?}");

        let asked = std::sync::atomic::AtomicBool::new(false);
        let line = remote_line_with(
            "0.7.0",
            || async { Ok(Update::OnDisk("9\u{1b}]0;x\u{7}".into(), None)) },
            |_| async {
                asked.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(String::new())
            },
        )
        .await;
        assert_eq!(
            line,
            "not updated: the machine reported a version this qoop cannot read"
        );
        assert!(
            !asked.load(std::sync::atomic::Ordering::SeqCst),
            "a daemon was asked to move onto something that is not a version"
        );
    }

    /// One remote's line: what was put there, what its daemon answered, and what the swap
    /// warned of, in that order and as text.
    #[tokio::test]
    async fn a_remote_line_says_what_was_done_and_what_failed() {
        let line = |put: std::io::Result<Update>, moved: Result<String, String>| async {
            remote_line_with("0.7.0", || async { put }, |_| async { moved }).await
        };
        assert_eq!(
            line(Ok(Update::NoRelease), Ok(String::new())).await,
            "no 0.7.0 release, left as it is"
        );
        // ssh's own reason, with nothing in it that could act on the terminal.
        let said = line(
            Err(std::io::Error::other(
                "\u{1b}[2Jssh: connect to host a port 22: Connection refused",
            )),
            Ok(String::new()),
        )
        .await;
        assert_eq!(
            said,
            "not updated: [2Jssh: connect to host a port 22: Connection refused"
        );
        assert_eq!(
            line(
                Ok(Update::OnDisk(
                    "0.7.0".into(),
                    Some("the bridge there was not reloaded".into())
                )),
                Ok("moving to 0.7.0 now".into()),
            )
            .await,
            "moving to 0.7.0 now\n  but the bridge there was not reloaded"
        );
        // A daemon that could not be asked is told as the failure it is, with the version
        // that is on its disk.
        assert_eq!(
            line(
                Ok(Update::OnDisk("0.7.0".into(), None)),
                Err("0.7.0 is on its disk, but its daemon was not asked to move: refused".into()),
            )
            .await,
            "0.7.0 is on its disk, but its daemon was not asked to move: refused"
        );
    }

    /// Putting the release over there waits out another update's lock and nothing else.
    #[tokio::test]
    async fn putting_a_release_on_a_machine_waits_out_a_busy_lock_and_stops_at_a_refusal() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let calls = AtomicU32::new(0);
        let done = put_on_with(
            |target, version| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!((target.as_str(), version.as_str()), ("box", "0.7.0"));
                    if n < 2 {
                        Err(std::io::Error::other(
                            "another update of qoop is in progress there; try again later",
                        ))
                    } else {
                        Ok(Update::OnDisk("0.7.0".into(), None))
                    }
                }
            },
            "box",
            "0.7.0",
        )
        .await;
        assert!(matches!(done, Ok(Update::OnDisk(..))));
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let calls = AtomicU32::new(0);
        let done = put_on_with(
            |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(std::io::Error::other("no qoop file to replace")) }
            },
            "box",
            "0.7.0",
        )
        .await;
        assert!(done.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a refusal was retried");
    }

    /// An already running version is said without asking anything of the daemon.
    #[tokio::test]
    async fn a_daemon_already_there_is_not_asked() {
        let (stream, _other) = tokio::io::duplex(64);
        let said = move_daemon(stream, BytesMut::new(), "0.7.0", "0.7.0", false, || async {
            Ok(String::new())
        })
        .await;
        assert_eq!(said, Ok("already 0.7.0".to_string()));
    }

    /// Another update holding the swap lock over there is waited out a few times, and only
    /// that: any other refusal ends at once.
    #[tokio::test]
    async fn a_busy_swap_lock_is_retried_and_nothing_else_is() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let busy = || {
            std::io::Error::other("another update of qoop is in progress there; try again later")
        };
        let calls = AtomicU32::new(0);
        let done = retry_busy(|| async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(busy())
            } else {
                Ok(Update::NoRelease)
            }
        })
        .await;
        assert_eq!(done.expect("done"), Update::NoRelease);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let calls = AtomicU32::new(0);
        let done = retry_busy(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(busy())
        })
        .await;
        assert!(done.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), super::BUSY_TRIES + 1);

        let calls = AtomicU32::new(0);
        let done = retry_busy(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::other("no qoop file to replace"))
        })
        .await;
        assert!(done.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A staged copy left by a killed install is swept once it is old, and one that may
    /// belong to an install running now is left.
    #[test]
    fn a_stale_staged_copy_is_swept_and_a_fresh_one_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoop");
        std::fs::write(&exe, "old binary").expect("old");
        let stale = dir.path().join(".qoop-update.stale");
        let fresh = dir.path().join(".qoop-update.fresh");
        for file in [&stale, &fresh] {
            std::fs::write(file, "half").expect("staged");
        }
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .expect("open")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .expect("age it");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: None,
        };
        install_to(&latest, "0.0.0", &exe, false).expect("install");
        assert!(!stale.exists(), "an old staged copy was kept");
        assert!(fresh.exists(), "a fresh staged copy was taken");
    }

    /// A release tarball whose `qoop` answers `--version` and `--hello` as a build with this
    /// codec generation (or `codec`) does.
    fn release_with_codec(dir: &std::path::Path, reported: &str, codec: u32) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("qoop");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --hello ]; then echo '{{\"version\":\"{reported}\",\"codec_generation\":{codec}}}'; exit 0; fi\necho \"qoop {reported}\"\n"
            ),
        )
        .expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let tarball = dir.join(MAC_ARM);
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&tarball)
            .arg("qoop")
            .current_dir(dir)
            .status()
            .expect("tar");
        assert!(status.success());
        format!("file://{}", tarball.display())
    }

    /// An installed binary that says it is `version`, in a directory of its own.
    fn installed(dir: &std::path::Path, version: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("qoop");
        std::fs::write(&exe, format!("#!/bin/sh\necho \"qoop {version}\"\n")).expect("old");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        exe
    }

    fn says(exe: &std::path::Path) -> String {
        let out = std::process::Command::new(exe)
            .arg("--version")
            .output()
            .expect("run it");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A release for `install_to`, packed under `dir/pack`, with a checksum beside it when
    /// asked and the right one unless `wrong`.
    fn release(dir: &std::path::Path, codec: u32, checksum: Option<bool>) -> Release {
        let pack = dir.join("pack");
        std::fs::create_dir_all(&pack).expect("pack dir");
        let asset_url = release_with_codec(&pack, "9.9.9", codec);
        let checksum_url = checksum.map(|right| {
            let sum = if right {
                let out = std::process::Command::new("shasum")
                    .args(["-a", "256"])
                    .arg(pack.join(MAC_ARM))
                    .output()
                    .expect("shasum");
                String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .next()
                    .expect("a digest")
                    .to_string()
            } else {
                "0".repeat(64)
            };
            let file = pack.join(format!("{MAC_ARM}.sha256"));
            std::fs::write(&file, format!("{sum}  {MAC_ARM}\n")).expect("sum");
            format!("file://{}", file.display())
        });
        Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url,
            checksum_url,
        }
    }

    fn leftovers(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".qoop"))
            .collect()
    }

    /// An unattended update is not one a person said yes to: a release with no checksum is
    /// not installed, and neither is one whose checksum is wrong, by hand or not.
    #[test]
    fn an_unattended_install_needs_a_checksum_that_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "0.0.0");
        let current = qoop_proto::CODEC_GENERATION;
        let none = release(dir.path(), current, None);
        assert!(install_to(&none, "0.0.0", &exe, true).is_err());
        let wrong = release(dir.path(), current, Some(false));
        assert!(install_to(&wrong, "0.0.0", &exe, true).is_err());
        assert!(install_to(&wrong, "0.0.0", &exe, false).is_err());
        assert_eq!(
            says(&exe),
            "qoop 0.0.0",
            "a refused download replaced the binary"
        );
        assert!(
            leftovers(dir.path()).is_empty(),
            "{:?}",
            leftovers(dir.path())
        );
        let right = release(dir.path(), current, Some(true));
        assert_eq!(
            install_to(&right, "0.0.0", &exe, true).expect("install"),
            Installed::Swapped
        );
        assert_eq!(says(&exe), "qoop 9.9.9");
        assert!(
            leftovers(dir.path()).is_empty(),
            "{:?}",
            leftovers(dir.path())
        );
    }

    /// A release that speaks another codec generation cannot be moved onto live. It is left
    /// where the old binary is, before anything is swapped, and said.
    #[test]
    fn an_unattended_install_stops_before_the_swap_on_another_codec_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "0.0.0");
        let other = release(dir.path(), qoop_proto::CODEC_GENERATION + 1, Some(true));
        match install_to(&other, "0.0.0", &exe, true).expect("not an error") {
            Installed::Skipped(why) => {
                assert!(why.contains("codec generation"), "{why}");
            }
            other => panic!("installed anyway: {other:?}"),
        }
        assert_eq!(says(&exe), "qoop 0.0.0");
        assert!(
            leftovers(dir.path()).is_empty(),
            "{:?}",
            leftovers(dir.path())
        );
    }

    /// Another update holding the lock is given way to, and its lock is its own: `--auto`
    /// returns quietly, a person's `qoop update` says so, and neither removes it.
    #[test]
    fn a_swap_lock_held_by_another_update_is_given_way_to() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "0.0.0");
        let latest = release(dir.path(), qoop_proto::CODEC_GENERATION, Some(true));
        let lock = dir.path().join(SWAP_LOCK);
        std::fs::create_dir(&lock).expect("their lock");
        assert!(matches!(
            install_to(&latest, "0.0.0", &exe, true),
            Ok(Installed::Skipped(why)) if why.contains("another update")
        ));
        let manual = install_to(&latest, "0.0.0", &exe, false).expect_err("busy");
        assert!(manual.to_string().contains("in progress"), "{manual}");
        assert!(lock.exists(), "the other update's lock was taken away");
        assert_eq!(says(&exe), "qoop 0.0.0");

        // One that was left by a killed update half an hour ago is not anyone's.
        std::fs::File::open(&lock)
            .expect("open")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .expect("age it");
        assert_eq!(
            install_to(&latest, "0.0.0", &exe, true).expect("install"),
            Installed::Swapped
        );
        assert!(!lock.exists(), "the lock outlived the swap");
    }

    /// Another update that put the release there while this one downloaded it is not swapped
    /// over again.
    #[test]
    fn a_release_already_on_disk_is_not_swapped_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "9.9.9");
        let before = std::fs::read(&exe).expect("read");
        let latest = release(dir.path(), qoop_proto::CODEC_GENERATION, Some(true));
        assert_eq!(
            install_to(&latest, "0.0.0", &exe, true).expect("install"),
            Installed::Already("9.9.9".to_string())
        );
        assert_eq!(std::fs::read(&exe).expect("read"), before);
        assert!(!dir.path().join(SWAP_LOCK).exists());
    }

    /// A directory the person cannot write to is no failure for the timer: nothing is
    /// downloaded or left behind. By hand it is an error that names the file.
    #[test]
    fn a_directory_that_cannot_be_written_is_skipped_quietly_when_unattended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "0.0.0");
        let latest = release(dir.path(), qoop_proto::CODEC_GENERATION, Some(true));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))
            .expect("chmod");
        let writable = std::fs::File::create(dir.path().join("probe")).is_ok();
        let auto = install_to(&latest, "0.0.0", &exe, true);
        let manual = install_to(&latest, "0.0.0", &exe, false);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        if writable {
            return; // running as someone for whom permissions do not apply
        }
        assert!(matches!(auto, Ok(Installed::Skipped(_))), "{auto:?}");
        assert!(manual.is_err());
        assert_eq!(says(&exe), "qoop 0.0.0");
    }

    /// What an install killed halfway leaves, a staged binary or an unpacked release, is swept
    /// once it is old.
    #[test]
    fn an_unpacked_release_left_by_a_killed_install_is_swept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "0.0.0");
        let stale = dir.path().join(".qoop-update.killed");
        std::fs::create_dir(&stale).expect("stale");
        std::fs::write(stale.join("qoop-aarch64-apple-darwin.tar.gz"), "half").expect("half");
        let fresh = dir.path().join(".qoop-update.running");
        std::fs::create_dir(&fresh).expect("fresh");
        std::fs::File::open(&stale)
            .expect("open")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .expect("age it");
        let latest = release(dir.path(), qoop_proto::CODEC_GENERATION, Some(true));
        install_to(&latest, "0.0.0", &exe, true).expect("install");
        assert!(!stale.exists(), "the killed install's directory was kept");
        assert!(fresh.exists(), "a running install's directory was taken");
    }

    fn release_of(version: &str) -> Result<Release, String> {
        Ok(Release {
            version: version.to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: String::new(),
            checksum_url: None,
        })
    }

    /// What the daemon is asked onto after the install step: the new version when it was
    /// installed, and this binary's own when nothing newer is out or the answer could not be
    /// read, so a daemon that missed its move is asked again within one tick.
    #[test]
    fn the_daemon_is_asked_again_even_with_nothing_newer_released() {
        let never = |_: &Release| -> std::io::Result<Installed> { panic!("installed") };
        assert_eq!(
            auto_version("0.9.2", &release_of("0.9.2"), never).expect("v"),
            "0.9.2"
        );
        assert_eq!(
            auto_version("0.9.2", &release_of("0.9.1"), never).expect("v"),
            "0.9.2"
        );
        assert_eq!(
            auto_version("0.9.2", &Err("offline".to_string()), never).expect("v"),
            "0.9.2"
        );
        let swapped = |_: &Release| Ok(Installed::Swapped);
        assert_eq!(
            auto_version("0.9.2", &release_of("0.9.3"), swapped).expect("v"),
            "0.9.3"
        );
        let already = |_: &Release| Ok(Installed::Already("0.9.4".to_string()));
        assert_eq!(
            auto_version("0.9.2", &release_of("0.9.3"), already).expect("v"),
            "0.9.4"
        );
        let skipped = |_: &Release| Ok(Installed::Skipped("busy".to_string()));
        assert_eq!(
            auto_version("0.9.2", &release_of("0.9.3"), skipped).expect("v"),
            "0.9.2"
        );
        let failed =
            |_: &Release| -> std::io::Result<Installed> { Err(std::io::Error::other("no")) };
        assert!(auto_version("0.9.2", &release_of("0.9.3"), failed).is_err());
    }

    /// What goes over the wire to the daemon, read from the other end of a pipe.
    async fn ask_and_read(running: &str, auto: bool) -> (Line, String) {
        use tokio::io::AsyncReadExt;
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let said = tokio::spawn(async move {
            let mut got = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match server.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        got.extend_from_slice(&chunk[..n]);
                        if got.windows(9).any(|w| w == b"\"version\"") {
                            break;
                        }
                    }
                }
            }
            String::from_utf8_lossy(&got).into_owned()
        });
        let line = move_daemon(client, BytesMut::new(), running, "0.9.3", auto, || async {
            Ok("0.9.3".to_string())
        })
        .await;
        (line, said.await.expect("read"))
    }

    /// The timer's ask carries both promises, a person's carries neither, and a daemon from
    /// before the promises is not asked at all.
    #[tokio::test]
    async fn only_the_unattended_ask_carries_the_promises_and_only_to_a_daemon_that_keeps_them() {
        let (line, sent) = ask_and_read("0.9.2", true).await;
        assert!(line.is_ok(), "{line:?}");
        assert!(
            sent.contains("\"keep_tabs\":true") && sent.contains("\"quiet\":true"),
            "{sent}"
        );

        let (_, sent) = ask_and_read("0.9.2", false).await;
        assert!(sent.contains("RestartWhenSafe"), "{sent}");
        assert!(
            !sent.contains("keep_tabs") && !sent.contains("quiet"),
            "{sent}"
        );

        let (line, sent) = ask_and_read("0.9.1", true).await;
        assert!(line.is_err(), "{line:?}");
        assert!(
            sent.is_empty(),
            "a daemon that would ignore the promises was asked: {sent}"
        );
        let (line, sent) = ask_and_read("0.9.1", false).await;
        assert!(
            line.is_ok() && sent.contains("RestartWhenSafe"),
            "{line:?} {sent}"
        );

        assert!(keeps_promises("0.9.2") && keeps_promises("0.10.0") && keeps_promises("1.0.0"));
        assert!(!keeps_promises("0.9.1") && !keeps_promises("") && !keeps_promises("junk"));
    }

    /// A program on disk that is the child: a shell script with this body.
    fn child(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("child");
        std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).expect("script");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        exe
    }

    /// The timer runs `update --auto`, waits for it and writes what it said to the log.
    #[tokio::test]
    async fn the_timer_runs_the_child_and_logs_what_it_said() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = child(dir.path(), "echo \"ran: $*\"; echo trouble >&2; exit 3");
        let log = dir.path().join("logs/update.log");
        auto_once(
            &exe,
            &dir.path().join("no-auto-update"),
            Some(&log),
            AUTO_LIMIT,
        )
        .await;
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.contains("ran: update --auto"), "{text}");
        assert!(text.contains("trouble"), "{text}");
        assert!(text.contains("ended: exit status: 3"), "{text}");
    }

    /// A child that hangs is killed once its time is up, and the timer goes on.
    #[tokio::test]
    async fn the_timer_kills_a_child_that_hangs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = child(dir.path(), "exec sleep 600");
        let log = dir.path().join("update.log");
        let started = std::time::Instant::now();
        auto_once(
            &exe,
            &dir.path().join("no-auto-update"),
            Some(&log),
            std::time::Duration::from_millis(200),
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(
            std::fs::read_to_string(&log)
                .expect("log")
                .contains("killed after")
        );
    }

    /// The opt out is a file, looked for at every tick: with it there nothing is started, and
    /// taking it away turns the checks back on without a restart of anything.
    #[tokio::test]
    async fn the_opt_out_file_stops_the_checks_and_removing_it_starts_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("ran");
        let exe = child(dir.path(), &format!("touch '{}'", marker.display()));
        let opt_out = dir.path().join("no-auto-update");
        std::fs::write(&opt_out, "").expect("opt out");
        let every = std::time::Duration::from_millis(50);
        let timer = tokio::spawn({
            let (exe, opt_out) = (exe.clone(), opt_out.clone());
            async move {
                auto_every(
                    &exe,
                    &opt_out,
                    None,
                    std::time::Duration::ZERO,
                    every,
                    AUTO_LIMIT,
                )
                .await;
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(!marker.exists(), "the child ran with the opt out in place");
        std::fs::remove_file(&opt_out).expect("remove");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        timer.abort();
        assert!(
            marker.exists(),
            "the child never ran once the opt out was gone"
        );
    }

    /// The log keeps the last hundred KB or two and not more.
    #[test]
    fn a_log_past_its_limit_is_moved_aside() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("update.log");
        std::fs::write(&log, vec![b'x'; (LOG_LIMIT + 1) as usize]).expect("big");
        note(Some(&log), "fresh");
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.contains("fresh") && text.len() < 200, "{text}");
        assert_eq!(
            std::fs::metadata(dir.path().join("update.log.old"))
                .expect("old")
                .len(),
            LOG_LIMIT + 1
        );
    }

    /// Unattended, a swap leaves the notch and the bridge alone and only marks them; by hand
    /// it puts them back at once. The mark is acted on later, once and only when nobody is
    /// connected.
    #[test]
    fn an_unattended_swap_marks_the_services_and_a_later_run_reloads_them_when_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = installed(dir.path(), "0.9.3");
        let mark = dir.path().join("rt/reload-pending");
        let reloaded = std::cell::Cell::new(0);
        let works = || {
            reloaded.set(reloaded.get() + 1);
            true
        };
        let refused = || {
            reloaded.set(reloaded.get() + 1);
            false
        };
        after_swap(&exe, true, &mark, |_| works()).expect("after");
        assert_eq!(
            reloaded.get(),
            0,
            "an unattended swap reloaded the services"
        );
        assert!(mark.exists());

        // Somebody is connected: nothing, and the mark stays for the next run.
        assert!(!reload_if_marked(&mark, false, works));
        assert_eq!((reloaded.get(), mark.exists()), (0, true));
        // Nobody is, but launchd refuses: tried, and the mark stays so the next run tries again.
        assert!(!reload_if_marked(&mark, true, refused));
        assert_eq!((reloaded.get(), mark.exists()), (1, true));
        // It works the next time: the mark goes.
        assert!(reload_if_marked(&mark, true, works));
        assert_eq!((reloaded.get(), mark.exists()), (2, false));
        // Nothing marked, nothing to do.
        assert!(!reload_if_marked(&mark, true, works));
        assert_eq!(reloaded.get(), 2);

        // By hand: reloaded at once and nothing is left over; refused, it is left for the
        // next unattended run.
        super::write_mark(&mark).expect("mark");
        after_swap(&exe, false, &mark, |_| works()).expect("after");
        assert_eq!(reloaded.get(), 3, "a swap by hand left the services alone");
        assert!(!mark.exists());
        after_swap(&exe, false, &mark, |_| refused()).expect("after");
        assert!(mark.exists(), "a refused reload by hand was forgotten");
    }

    /// A binary the release script did not build never updates itself, whatever starts it.
    #[test]
    fn a_build_without_the_release_mark_exits_before_the_network() {
        if release_built() {
            return;
        }
        // No release base is set for this test: reaching for one would be an error here.
        run(&["qoop".into(), "update".into(), "--auto".into()]).expect("quiet exit");
        assert!(
            run(&[
                "qoop".into(),
                "update".into(),
                "--auto".into(),
                "--check".into()
            ])
            .is_err()
        );
    }
}

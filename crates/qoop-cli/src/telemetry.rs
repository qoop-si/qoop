//! `qoop telemetry [status|show|on|off]`: the usage counts (ADR-023). `show` prints every day
//! not yet sent exactly as it would be sent; `off` stops it at once and deletes what was
//! counted; `on` turns it back on and shows the notice line again.

use qoop_runtime::config::{TelemetrySetting, set_telemetry, telemetry_from};
use qoop_runtime::control::{ControlRequest, ControlResponse, TelemetryOp};
use qoop_runtime::paths;
use qoop_runtime::telemetry::{NOTICE, NOTICE_HOLD, env_opted_out, mark_noticed, noticed_path};

const USAGE: &str = "usage: qoop telemetry [status|show|on|off]";

pub fn run(args: &[String]) -> std::io::Result<()> {
    match (args.get(2).map(String::as_str), args.len()) {
        (None | Some("status"), ..=3) => status(TelemetryOp::Status),
        (Some("show"), 3) => status(TelemetryOp::Show),
        (Some("off"), 3) => off(),
        (Some("on"), 3) => on(),
        _ => Err(crate::usage(USAGE)),
    }
}

/// The running daemon's answer, `None` when no daemon runs.
fn tell(op: TelemetryOp) -> std::io::Result<Option<ControlResponse>> {
    let (terminal, token) = crate::task::here();
    let request = ControlRequest::Telemetry {
        id: crate::request_id("telemetry"),
        terminal,
        token,
        action: op,
    };
    match crate::task::ask(request, std::time::Duration::from_secs(15)) {
        Ok(ControlResponse::Failed { reason, .. })
            if qoop_runtime::control::too_old_for(&reason, Some("Telemetry")) =>
        {
            Err(std::io::Error::other(
                "the running qoop is older than this one: `qoop restart`, then again",
            ))
        }
        Ok(ControlResponse::Failed { reason, .. }) => Err(std::io::Error::other(reason)),
        Ok(answer) => Ok(Some(answer)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn status(op: TelemetryOp) -> std::io::Result<()> {
    let answer = tell(op)?;
    let (state, why, counting, days) = match answer {
        Some(ControlResponse::Telemetry {
            state,
            why,
            counting,
            days,
            ..
        }) => (state, why, counting, days),
        Some(_) => return Err(std::io::Error::other("the daemon answered something else")),
        None => {
            let (state, why) = match telemetry_from(&qoop_runtime::config::path()) {
                TelemetrySetting::Off(at) => ("off", Some(format!("{at} in config.toml"))),
                TelemetrySetting::Unreadable(why) => ("paused", Some(why)),
                TelemetrySetting::On if !noticed_path().exists() => (
                    "off",
                    Some("the notice line has not been shown yet".to_string()),
                ),
                TelemetrySetting::On => ("on", None),
            };
            (state.to_string(), why, true, None)
        }
    };
    if op == TelemetryOp::Show {
        let days = days.unwrap_or_else(|| serde_json::json!([]));
        println!(
            "{}",
            serde_json::to_string_pretty(&days).unwrap_or_default()
        );
        return Ok(());
    }
    match why {
        Some(why) => println!("usage counts: {state} ({why})"),
        None => println!("usage counts: {state}"),
    }
    if !counting {
        println!("this qoop keeps no counts: only a release build does");
    }
    if env_opted_out() {
        println!(
            "off because DO_NOT_TRACK or QOOP_TELEMETRY is set: unset it, then qoop telemetry on"
        );
    }
    println!(
        "what is sent: qoop telemetry show; about it: https://qoop-si.github.io/qoop/#telemetry"
    );
    Ok(())
}

/// Off at once in the daemon (counts, id and notice mark deleted, a send stopped), then in the
/// file. The deletion never waits on the file: an unwritable or broken `config.toml` is said,
/// and counting stays off anyway.
fn off() -> std::io::Result<()> {
    let (said, written) = off_everywhere();
    match written {
        Ok(()) => println!("usage counts: off; everything counted was deleted"),
        Err(e) => {
            println!("usage counts: off for now; everything counted was deleted");
            eprintln!("qoop: config.toml was not changed: {e}");
        }
    }
    if let Some(said) = said {
        eprintln!("qoop: the running qoop did not answer the off: {said}");
    }
    Ok(())
}

/// Off in the daemon, and whatever the daemon said, off here too: the counts deleted from the
/// store when it did not take the off, the notice mark deleted, `telemetry = false` written.
/// Answers what went wrong with the daemon, if anything, and the file write's result.
fn off_everywhere() -> (Option<String>, Result<(), String>) {
    let said = match tell(TelemetryOp::Off) {
        // A daemon that keeps no counts left the store as it was: cleared here.
        Ok(Some(ControlResponse::Telemetry {
            counting: false, ..
        })) => {
            let _ = purge_without_daemon();
            None
        }
        Ok(Some(_)) => None,
        Ok(None) => {
            let _ = purge_without_daemon();
            None
        }
        Err(e) => {
            let _ = purge_without_daemon();
            Some(e.to_string())
        }
    };
    let _ = std::fs::remove_file(noticed_path());
    (said, set_telemetry(&qoop_runtime::config::path(), false))
}

fn purge_without_daemon() -> std::io::Result<()> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    if let Some(store) = crate::existing_store(&dir)? {
        store.telemetry_purge().map_err(std::io::Error::other)?;
    }
    Ok(())
}

/// On in the file and the daemon, then the notice line again, held on screen before it counts
/// as seen. Refused inside a qoop terminal: an agent does not undo the owner's off.
fn on() -> std::io::Result<()> {
    turn_on().map_err(std::io::Error::other)?;
    println!("{NOTICE}");
    std::thread::sleep(NOTICE_HOLD);
    if mark_noticed() {
        tell(TelemetryOp::On)?;
        println!("usage counts: on");
    } else {
        println!("usage counts: on once the notice is shown in a terminal (run qoop)");
    }
    Ok(())
}

/// The settings row: off as `qoop telemetry off` does it; on in the file and the daemon, with
/// counting starting once the notice line was shown in a terminal again (the next `qoop`).
pub fn switch(on: bool) -> Result<(), String> {
    if on { turn_on() } else { off_everywhere().1 }
}

/// On in the file and the daemon: refused inside a qoop terminal (an agent does not undo the
/// owner's off; a guard, not a boundary) and while the environment opts out.
fn turn_on() -> Result<(), String> {
    if qoop_runtime::telemetry::in_qoop_terminal() {
        return Err(
            "usage counts are turned on by the owner, not from inside a qoop terminal".into(),
        );
    }
    if env_opted_out() {
        return Err("DO_NOT_TRACK or QOOP_TELEMETRY is set: unset it first".into());
    }
    set_telemetry(&qoop_runtime::config::path(), true)?;
    tell(TelemetryOp::On).map_err(|e| e.to_string())?;
    Ok(())
}

/// The notice line, the first time a person starts qoop in a terminal: printed, held on screen
/// for `NOTICE_HOLD` before the interface takes the screen, and only then marked as seen and
/// counting started. Never for an agent in a qoop terminal, a script, an opt out in the
/// environment, or a file that says off.
pub fn notice_once() {
    let unseen = matches!(
        telemetry_from(&qoop_runtime::config::path()),
        TelemetrySetting::On
    ) && !noticed_path().exists();
    if !unseen || !qoop_runtime::telemetry::may_notice() {
        return;
    }
    // Written as on before the line shows: a file that cannot be written cannot hold an off
    // either, so with one nothing ever starts counting (an off that failed to write it stays).
    if set_telemetry(&qoop_runtime::config::path(), true).is_err() {
        return;
    }
    println!("{NOTICE}");
    std::thread::sleep(NOTICE_HOLD);
    // `On`, not a plain look: an off sent through the socket earlier and undone in the file
    // since would otherwise delete the mark again at once.
    if mark_noticed() {
        let _ = tell(TelemetryOp::On);
    }
}

/// What every command does first: an opt out in the environment is written down for good, so
/// the daemon, which reads no environment, stops counting too. qoop always goes on.
pub fn honour_env() {
    if !env_opted_out() {
        return;
    }
    if let Some(warning) = qoop_runtime::telemetry::opt_out_from_env() {
        eprintln!("{warning}");
    }
    // Every time, the config off or not: without the mark nothing counts, and the next daemon
    // that looks deletes whatever an earlier one left before it could send it.
    let _ = std::fs::remove_file(noticed_path());
    // Every time, not only the first: a running daemon stops now (a send in flight with it)
    // rather than at its next look. One local round trip.
    match tell(TelemetryOp::Off) {
        Ok(Some(ControlResponse::Telemetry { counting: true, .. })) => {}
        // No daemon, an error, or one that keeps no counts: the store is cleared here.
        _ => {
            let _ = purge_without_daemon();
        }
    }
}

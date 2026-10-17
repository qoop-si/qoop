//! `qoop show`: put a result on the owner's phone, from inside a qoop terminal.
//!
//! The daemon records the file's path (never a copy) or the url, and the phone lists it and
//! fetches the file when the owner taps it. Only the CLI's half of the checks is here; the daemon
//! checks again, since a client on the socket can skip this.

use std::time::Duration;

use qoop_runtime::control::{ControlRequest, ControlResponse, too_old_for};
use qoop_runtime::shows;

use crate::usage;

const USAGE: &str = "usage: qoop show <file|url> [--note <text>]
  put a screenshot, a recording, a report or a url on the owner's phone;
  a http://localhost:<port> url opens there as a live preview of the Mac's dev server";

pub fn run(args: &[String]) -> std::io::Result<()> {
    let rest = &args[args.len().min(2)..];
    let mut target = None;
    let mut note = None;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--note" => note = Some(words.next().ok_or_else(|| usage(USAGE))?.clone()),
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ if target.is_none() => target = Some(word.clone()),
            _ => return Err(usage(USAGE)),
        }
    }
    let target = target.ok_or_else(|| usage(USAGE))?;
    let (Some(terminal), Some(token)) = crate::task::here() else {
        return Err(std::io::Error::other(
            "qoop show runs inside a qoop terminal",
        ));
    };
    // Checked here too for a daemon older than the preview, which would keep any loopback url.
    // The note is made one line and cut by the daemon, which a raw client cannot skip.
    let target = if shows::is_url(&target) {
        if let Some(Err(reason)) = shows::local_port(&target) {
            return Err(std::io::Error::other(reason));
        }
        target
    } else {
        std::path::absolute(&target)?.to_string_lossy().into_owned()
    };
    let request = ControlRequest::ShowWrite {
        id: crate::request_id("show"),
        terminal,
        token,
        target,
        note,
    };
    match crate::task::ask(request, Duration::from_secs(10))? {
        ControlResponse::ShowWritten { show, .. } => {
            println!("shown {show}");
            Ok(())
        }
        ControlResponse::Failed { reason, .. } if too_old_for(&reason, Some("ShowWrite")) => Err(
            std::io::Error::other("the running qoop daemon is older; run `qoop restart`"),
        ),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

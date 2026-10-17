//! `qoop show`: put a result on the owner's phone, from inside a qoop terminal.
//!
//! The daemon records the file's path (never a copy) or the url, and the phone lists it and
//! fetches the file when the owner taps it. Only the CLI's half of the checks is here; the daemon
//! checks again, since a client on the socket can skip this.

use std::net::IpAddr;
use std::time::Duration;

use qoop_runtime::control::{ControlRequest, ControlResponse, too_old_for};

use crate::usage;

const USAGE: &str = "usage: qoop show <file|url> [--note <text>]
  put a screenshot, a recording, a report or a public url on the owner's phone";

/// The host of an `http` or `https` url, or `None` when it is neither.
fn host_of(target: &str) -> Option<&str> {
    let lower = target.get(..8).unwrap_or(target).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &target[8..]
    } else if lower.starts_with("http://") {
        &target[7..]
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    Some(match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    })
}

/// Whether the phone could never reach this host: it is the Mac's own.
fn loopback(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let mapped = |ip: IpAddr| match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<IpAddr>()
            .map(mapped)
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

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
    let target = match host_of(&target) {
        Some(host) if loopback(host) => {
            return Err(std::io::Error::other(
                "the phone cannot open a local address; show a screenshot instead",
            ));
        }
        Some(_) => target,
        None => std::path::absolute(&target)?.to_string_lossy().into_owned(),
    };
    // The note is made one line and cut by the daemon, which a raw client cannot skip.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_address_is_told_apart_from_a_public_one() {
        for local in [
            "http://localhost:3000/x",
            "https://LOCALHOST",
            "http://127.0.0.1/",
            "http://127.8.9.1:80",
            "http://[::1]:8080/a",
            "http://user@127.0.0.1/",
            "http://localhost./",
            "http://app.localhost:5173",
            "http://0.0.0.0:8000",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            assert!(loopback(host_of(local).unwrap()), "{local}");
        }
        for public in [
            "https://github.com/qoop-si/qoop/pull/1",
            "http://example.com?x=localhost",
            "https://10.0.0.2:8443",
        ] {
            assert!(!loopback(host_of(public).unwrap()), "{public}");
        }
        assert_eq!(host_of("shot.png"), None);
        assert_eq!(host_of("javascript:alert(1)"), None);
    }
}

//! The daemon's side of `qoop show`: a result an agent puts on the owner's phone.
//!
//! An agent shows a file or a url with its own terminal's token. The daemon records the
//! canonical path, never a copy, and later serves that one path in chunks to whoever asks
//! by the show's id. No rule limits which path may be shown: an agent holding a terminal's
//! token can already type `cat` into any terminal and read its screen, so a root check
//! would protect nothing. The boundary is that a fetch reads only a recorded path.

use std::path::Path;
use std::sync::Arc;

use qoop_store::ShowRow;

use crate::agents::AgentRegistry;
use crate::asks::failed;
use crate::control::{ASK_FIELD_MAX, ControlResponse, SHOW_CHUNK, SHOW_MOST, ShowInfo};
use crate::daemon::{ConnectionRegistry, PROBE_DEADLINE, TerminalRegistry};
use crate::store::{StoreHandle, now};

/// How many shows a read carries.
const SHOWN: usize = 50;

/// The longest url kept.
const URL_MOST: usize = 2048;

/// What a fetch says when the recorded path holds no file now.
const GONE: &str = "the file is gone";

/// What a fetch says when the recorded path holds something else now.
const CHANGED: &str = "the file changed, show it again";

/// What a check of a path says when the disk under it did not answer in time.
const UNREADABLE: &str = "the file is not readable right now";

/// The extensions shown as a picture or a clip; everything else is a file.
const IMAGES: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "heic"];
const VIDEOS: [&str; 3] = ["mp4", "mov", "m4v"];

/// An `http` or `https` url with a host, checked here too: a client on the socket can skip
/// the CLI.
pub fn is_url(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"));
    rest.is_some_and(|r| {
        r.split(['/', '?', '#'])
            .next()
            .is_some_and(|h| !h.is_empty())
    }) && target.len() <= URL_MOST
        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The port of a url on the Mac's own loopback, which the phone opens through a tunnel:
/// `None` for any other url, a refusal for a loopback url said in a way the phone's own
/// `localhost` would not mean the same (`0.0.0.0`, `*.localhost`, a low or missing port).
pub fn local_port(url: &str) -> Option<Result<u16, String>> {
    let lower = url.to_ascii_lowercase();
    let (plain, rest) = match lower.strip_prefix("http://") {
        Some(rest) => (true, rest),
        None => (false, lower.strip_prefix("https://")?),
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    let (host, port) = match hostport.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map(|(h, p)| (h, p.strip_prefix(':')))?,
        None => match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        },
    };
    let ip = host.parse::<std::net::IpAddr>().ok().map(|ip| match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, std::net::IpAddr::V4),
        v4 => v4,
    });
    let bare = host.strip_suffix('.').unwrap_or(host);
    let loopback = bare == "localhost"
        || bare.ends_with(".localhost")
        || ip.is_some_and(|ip| ip.is_loopback() || ip.is_unspecified());
    if !loopback {
        return None;
    }
    // Digits as written, no sign and no leading zero: one port has one spelling.
    let port = port
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()) && !p.starts_with('0'))
        .and_then(|p| p.parse::<u16>().ok());
    Some(match port {
        Some(port)
            if plain
                && port >= 1024
                && ["localhost", "127.0.0.1", "::1"].contains(&host)
                && authority == hostport =>
        {
            Ok(port)
        }
        _ => Err(format!(
            "show it as http://localhost:{}",
            port.map_or_else(|| "<port>".to_string(), |p| p.to_string())
        )),
    })
}

/// `owner/name` of a GitHub `url` (`git@github.com:o/n`, `ssh://`, `https://` with or without
/// a user, an optional `.git` and trailing `/`). The host is read up to the first `/`, `?` or
/// `#`, so `https://evil.com#@github.com/o/n` is `evil.com`; a query or fragment anywhere is
/// no repository.
pub fn github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let at = rest.find(['/', '?', '#'])?;
            let (host, path) = rest.split_at(at);
            (host, path.strip_prefix('/')?)
        }
        None => url.split_once(':')?,
    };
    let odd = |c: char| matches!(c, '/' | '?' | '#' | '\\' | ' ') || c.is_control();
    if host.contains(odd) || path.contains(['?', '#']) {
        return None;
    }
    let host = host.rsplit('@').next()?;
    let host = host.split(':').next()?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let plain = |s: &str| !s.is_empty() && !s.contains('/');
    (host.eq_ignore_ascii_case("github.com") && plain(owner) && plain(name))
        .then(|| format!("{owner}/{name}"))
}

/// The port a tunnel for `show` dials: only a loopback url recorded by a show.
pub(crate) async fn port_of(show: u64, store: &StoreHandle) -> Result<u16, String> {
    match store.run(move |s| s.show(show as i64)).await {
        Ok(Some(row)) if row.kind == "url" => match local_port(&row.target) {
            Some(Ok(port)) => Ok(port),
            _ => Err(format!("show {show} is not on this Mac's localhost")),
        },
        Ok(Some(_)) => Err(format!("show {show} is a file, not a url")),
        Ok(None) => Err(format!("no show {show}")),
        Err(e) => Err(e.to_string()),
    }
}

fn kind_of(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if IMAGES.contains(&ext.as_str()) {
        "image"
    } else if VIDEOS.contains(&ext.as_str()) {
        "video"
    } else {
        "file"
    }
}

/// A file to show: its canonical path, kind and size, or why it is refused.
fn file(path: &str) -> Result<(String, &'static str, u64), String> {
    let real = std::fs::canonicalize(path).map_err(|_| format!("no file {path}"))?;
    let meta = std::fs::metadata(&real).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    if meta.len() > SHOW_MOST {
        return Err(format!("{path} is over {} MiB", SHOW_MOST >> 20));
    }
    let kind = kind_of(&real);
    let real = real
        .into_os_string()
        .into_string()
        .map_err(|_| format!("{path} is not a UTF-8 path"))?;
    Ok((real, kind, meta.len()))
}

/// Record a show for `terminal`, whose token the caller has already checked.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write(
    id: String,
    terminal: u32,
    target: String,
    note: Option<String>,
    space: Option<String>,
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) -> ControlResponse {
    let Some(generation) = agents.agent(terminal).map(|a| a.belief.generation.0) else {
        return failed(id, format!("terminal {terminal} has ended"));
    };
    let (target, kind, size) = if is_url(&target) {
        if let Some(Err(reason)) = local_port(&target) {
            return failed(id, reason);
        }
        (target, "url", 0)
    } else if target.starts_with('/') {
        // On the probe pool, keyed by the path: a hung mount degrades only itself.
        let key = target.clone();
        match crate::blocking::probe(&key, PROBE_DEADLINE, move || file(&target)).await {
            Ok(Ok(found)) => found,
            Ok(Err(reason)) => return failed(id, reason),
            Err(_) => return failed(id, UNREADABLE),
        }
    } else {
        return failed(
            id,
            "qoop shows an absolute file path or an http or https url",
        );
    };
    let note = note
        .map(|n| {
            qoop_store::task::one_line(&n)
                .chars()
                .take(ASK_FIELD_MAX)
                .collect::<String>()
        })
        .filter(|n| !n.is_empty());
    let session = terminals.session_of(terminal);
    let row = ShowRow {
        id: 0,
        at: now(),
        boot: crate::daemon::boot().to_string(),
        terminal: i64::from(terminal),
        generation: generation as i64,
        space,
        title: terminals
            .session_title(session.as_deref())
            .or_else(|| terminals.name_of(terminal)),
        kind: kind.to_string(),
        target,
        size: size as i64,
        note,
    };
    match store.run(move |s| s.add_show(&row)).await {
        Ok(show) => {
            connections.shows_changed();
            ControlResponse::ShowWritten {
                id,
                show: show as u64,
            }
        }
        Err(e) => failed(id, e.to_string()),
    }
}

fn info(row: ShowRow, terminals: &TerminalRegistry, agents: &AgentRegistry) -> ShowInfo {
    let terminal = u32::try_from(row.terminal).unwrap_or(0);
    let current = row.boot == crate::daemon::boot()
        && terminals.workspace_of(terminal).is_some()
        && agents.agent(terminal).map(|a| a.belief.generation.0 as i64) == Some(row.generation);
    let name = if row.kind == "url" {
        row.target
    } else {
        Path::new(&row.target)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    ShowInfo {
        show: row.id as u64,
        kind: row.kind,
        name,
        note: row.note,
        size: row.size as u64,
        at: row.at,
        terminal,
        current,
        space: row.space,
        title: row.title,
    }
}

pub(crate) async fn read(
    id: String,
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    store: &StoreHandle,
) -> ControlResponse {
    match store.run(|s| s.shows(SHOWN)).await {
        Ok(rows) => ControlResponse::Shows {
            id,
            shows: rows
                .into_iter()
                .map(|row| info(row, terminals, agents))
                .collect(),
        },
        Err(e) => failed(id, e.to_string()),
    }
}

/// Up to `len` bytes of `path` from `offset`, with its size and modification time now.
/// Opened without blocking, so a fifo put in the file's place cannot hold a thread, and
/// checked on the open file, so what is read is what was checked.
fn chunk(path: &str, offset: u64, len: u32) -> Result<(Vec<u8>, u64, u64), String> {
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(GONE.into()),
        Err(e) => return Err(e.to_string()),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > SHOW_MOST {
        return Err(CHANGED.into());
    }
    let total = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    let want = total.saturating_sub(offset).min(u64::from(len)) as usize;
    let mut data = vec![0; want];
    let mut got = 0;
    while got < want {
        match file.read_at(&mut data[got..], offset + got as u64) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    data.truncate(got);
    Ok((data, total, mtime))
}

pub(crate) async fn fetch(
    id: String,
    show: u64,
    offset: u64,
    len: u32,
    store: &StoreHandle,
) -> ControlResponse {
    use base64::Engine as _;
    let row = match store.run(move |s| s.show(show as i64)).await {
        Ok(Some(row)) => row,
        Ok(None) => return failed(id, format!("no show {show}")),
        Err(e) => return failed(id, e.to_string()),
    };
    if row.kind == "url" {
        return failed(id, format!("show {show} is a url, not a file"));
    }
    let len = len.min(SHOW_CHUNK);
    let key = row.target.clone();
    match crate::blocking::probe(&key, PROBE_DEADLINE, move || {
        chunk(&row.target, offset, len)
    })
    .await
    {
        Ok(Ok((data, total, mtime))) => ControlResponse::ShowChunk {
            id,
            data: base64::engine::general_purpose::STANDARD.encode(data),
            total,
            mtime,
        },
        Ok(Err(reason)) => failed(id, reason),
        Err(_) => failed(id, UNREADABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loopback_url_is_taken_only_as_the_phone_can_open_it() {
        for (url, port) in [
            ("http://localhost:3000", 3000),
            ("http://LOCALHOST:5173/app?x=1", 5173),
            ("http://127.0.0.1:8080/", 8080),
            ("http://[::1]:65535#top", 65535),
        ] {
            assert_eq!(local_port(url), Some(Ok(port)), "{url}");
        }
        for url in [
            "http://0.0.0.0:3000",
            "http://app.localhost:5173",
            "http://localhost",
            "http://localhost:80",
            "http://localhost:99999",
            "https://localhost:3000",
            "http://127.8.9.1:3000",
            "http://[::ffff:127.0.0.1]:3000",
            "http://[::]:3000",
            "http://localhost.:3000",
            "http://evil.com:80@localhost:3000",
            "http://localhost:+3000",
            "http://localhost:03000",
        ] {
            assert!(matches!(local_port(url), Some(Err(_))), "{url}");
        }
        assert_eq!(
            local_port("http://0.0.0.0:3000"),
            Some(Err("show it as http://localhost:3000".into()))
        );
        for url in [
            "https://github.com/qoop-si/qoop/pull/1",
            "http://example.com?x=localhost",
            "https://10.0.0.2:8443",
        ] {
            assert_eq!(local_port(url), None, "{url}");
        }
    }
}

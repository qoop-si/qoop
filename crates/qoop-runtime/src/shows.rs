//! The daemon's side of `qoop show`: a result an agent puts on the owner's phone.
//!
//! An agent shows a file or a url with its own terminal's token. The daemon records the
//! canonical path, never a copy, and later serves that one path in chunks to whoever asks
//! by the show's id. No rule limits which path may be shown: an agent holding a terminal's
//! token can already type `cat` into any terminal and read its screen, so a root check
//! would protect nothing. The boundary is that a fetch reads only a recorded path.
//!
//! The phone records one too, for a path or url the owner tapped in a chat (`open`). That
//! needs no token: what it opens is only what the phone can view, and a load nobody tapped
//! (an inline picture) only from where agents put their results.

use std::path::{Path, PathBuf};
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

/// The other files the phone opens from a chat; html, svg, xml and the rest open on the Mac.
const VIEWED: [&str; 24] = [
    "pdf", "md", "txt", "log", "json", "csv", "rs", "ts", "tsx", "js", "jsx", "py", "go", "rb",
    "sh", "swift", "kt", "java", "c", "h", "cpp", "toml", "yaml", "yml",
];

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

/// A file to show: its canonical path, kind, size and modification time (unix milliseconds),
/// or why it is refused.
fn file(path: &str) -> Result<(String, &'static str, u64, i64), String> {
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
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    Ok((real, kind, meta.len(), mtime))
}

/// The terminal's title as the Inbox shows it.
fn title_of(terminal: u32, terminals: &TerminalRegistry) -> Option<String> {
    let session = terminals.session_of(terminal);
    terminals
        .session_title(session.as_deref())
        .or_else(|| terminals.name_of(terminal))
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
            Ok(Ok((real, kind, size, _))) => (real, kind, size),
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
    let row = ShowRow {
        id: 0,
        at: now(),
        boot: crate::daemon::boot().to_string(),
        terminal: i64::from(terminal),
        generation: generation as i64,
        space,
        title: title_of(terminal, terminals),
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

/// Where a path tapped in `terminal`'s chat points: `~` is this Mac's home, a relative path
/// is under the terminal's folder.
fn resolve(target: &str, cwd: Option<&str>) -> Result<String, String> {
    if target.starts_with('/') {
        return Ok(target.to_string());
    }
    let (base, rest) = match target.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => (
            std::env::var("HOME").map_err(|_| "this Mac has no home folder set")?,
            rest.trim_start_matches('/'),
        ),
        _ => (
            cwd.ok_or("the terminal's folder is not known yet")?
                .to_string(),
            target,
        ),
    };
    Ok(Path::new(&base).join(rest).to_string_lossy().into_owned())
}

/// The largest picture loaded without a tap.
const AUTO_MOST: u64 = 2 << 20;

/// Whether `real` is where an agent puts what it made: under the terminal's folder (`cwd`),
/// `/tmp` or `$TMPDIR` (`tmps`), all canonical, as `home` is. A root that is `/` or holds the
/// home folder counts for nothing, since agents are often started there; with no home known,
/// neither does the terminal's folder.
fn rooted(real: &Path, cwd: Option<&Path>, tmps: &[PathBuf], home: Option<&Path>) -> bool {
    cwd.filter(|_| home.is_some())
        .into_iter()
        .chain(tmps.iter().map(PathBuf::as_path))
        .filter(|root| root.parent().is_some() && !home.is_some_and(|h| h.starts_with(root)))
        .any(|root| real.starts_with(root))
}

/// A file the phone asked to open: `file`'s checks, then a kind the phone views, then for
/// `auto` a picture of at most [`AUTO_MOST`] in a place agents put results in.
fn asked(
    path: &str,
    cwd: Option<&str>,
    auto: bool,
) -> Result<(String, &'static str, u64, i64), String> {
    let (real, kind, size, mtime) = file(path)?;
    let ext = Path::new(&real)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if kind == "file" && !VIEWED.contains(&ext.as_str()) {
        return Err(format!("{path} opens on the Mac"));
    }
    if auto {
        let canonical = |p: &Path| std::fs::canonicalize(p).ok();
        let home = std::env::var_os("HOME").and_then(|h| canonical(Path::new(&h)));
        let cwd = cwd.and_then(|c| canonical(Path::new(c)));
        let tmps: Vec<PathBuf> = [Path::new("/tmp"), &std::env::temp_dir()]
            .into_iter()
            .filter_map(canonical)
            .collect();
        let near = rooted(Path::new(&real), cwd.as_deref(), &tmps, home.as_deref());
        if kind != "image" || size > AUTO_MOST || !near {
            return Err(format!("{path} is opened only on a tap"));
        }
    }
    Ok((real, kind, size, mtime))
}

/// Record a show the phone asked for from `terminal`'s chat, answered as `Shows` with that
/// one. Not news and not listed: it is the phone's own, so it tells no one, and its row is
/// found by id only.
pub(crate) async fn open(
    id: String,
    terminal: u32,
    target: String,
    auto: bool,
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    store: &StoreHandle,
) -> ControlResponse {
    let Some(generation) = agents.agent(terminal).map(|a| a.belief.generation.0) else {
        return failed(id, format!("terminal {terminal} has ended"));
    };
    let (target, kind, size, mtime) = if is_url(&target) {
        if auto {
            return failed(id, "a url is opened only on a tap");
        }
        if let Some(Err(reason)) = local_port(&target) {
            return failed(id, reason);
        }
        (target, "url", 0, 0)
    } else {
        let cwd = terminals.cwd_of(terminal);
        let path = match resolve(&target, cwd.as_deref()) {
            Ok(path) => path,
            Err(reason) => return failed(id, reason),
        };
        let key = path.clone();
        match crate::blocking::probe(&key, PROBE_DEADLINE, move || {
            asked(&path, cwd.as_deref(), auto)
        })
        .await
        {
            Ok(Ok(found)) => found,
            Ok(Err(reason)) => return failed(id, reason),
            Err(_) => return failed(id, UNREADABLE),
        }
    };
    let row = ShowRow {
        id: 0,
        at: now(),
        boot: crate::daemon::boot().to_string(),
        terminal: i64::from(terminal),
        generation: generation as i64,
        space: None,
        title: title_of(terminal, terminals),
        kind: kind.to_string(),
        target,
        size: size as i64,
        note: None,
    };
    match store.run(move |s| s.ask_show(&row, mtime)).await {
        Ok(row) => ControlResponse::Shows {
            id,
            shows: vec![info(row, terminals, agents)],
        },
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
    fn a_root_that_holds_the_home_folder_counts_for_nothing() {
        let home = Some(Path::new("/Users/me"));
        let shot = Path::new("/Users/me/work/shot.png");
        let tmp = [PathBuf::from("/private/tmp")];
        assert!(rooted(shot, Some(Path::new("/Users/me/work")), &tmp, home));
        for cwd in ["/", "/Users", "/Users/me"] {
            assert!(!rooted(shot, Some(Path::new(cwd)), &tmp, home), "{cwd}");
        }
        // `$TMPDIR` set to the home folder, or to `/`.
        for tmpdir in ["/Users/me", "/"] {
            let tmps = [PathBuf::from("/private/tmp"), PathBuf::from(tmpdir)];
            assert!(!rooted(shot, None, &tmps, home), "{tmpdir}");
        }
        // No home known: the terminal's folder is no root, `/tmp` still is.
        assert!(!rooted(shot, Some(Path::new("/Users/me/work")), &tmp, None));
        assert!(rooted(Path::new("/private/tmp/a.png"), None, &tmp, None));
    }

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

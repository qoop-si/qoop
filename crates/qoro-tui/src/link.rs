//! One remote daemon, reached through the system `ssh` running `qoro relay` over there.
//!
//! The relay speaks the same protocol as a local socket, so what this returns is a byte
//! stream that has already done its handshake. Everything above it treats the remote as a
//! daemon like any other.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use qoro_remote::SshTransport;
use qoro_remote::saved::{
    ListedSession, MARKER_LIMIT, RELAY_MARKER, Remote, parse_sessions, remote_command, shell_quote,
};
use qoro_runtime::client;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// The stream to a remote daemon: ssh's stdout to read, its stdin to write.
pub type Upstream = tokio::io::Join<BufReader<ChildStdout>, ChildStdin>;

/// How long a connection may take from spawning ssh to the daemon's welcome. A host that
/// answers nothing in that time is shown as offline and tried again later, rather than
/// holding a space in "connecting" forever.
const OPEN_DEADLINE: Duration = Duration::from_secs(20);

pub struct Link {
    pub stream: Upstream,
    pub leftover: BytesMut,
    /// Held so the connection lives exactly as long as this does: dropping it kills ssh.
    pub child: Child,
    /// The last line ssh wrote to stderr, which is the best account there is of why a
    /// connection ended. Empty when stderr goes to the terminal instead.
    pub said: Arc<Mutex<String>>,
}

/// A private directory for ssh's control sockets, and the path pattern inside it.
///
/// `%C` is a fixed-length hash of the connection, so the path has a known length: a unix
/// socket path is capped near 104 bytes and ssh appends a random suffix while it creates
/// the master. A home deep enough to break that gets no connection sharing rather than a
/// connection that fails.
pub fn control_path() -> Option<String> {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".cache"))
                .ok()
        })?
        .join("qoro");
    let dir = base.to_str()?.to_string();
    // cm-, 40 hex characters, and the 17 byte suffix ssh adds while binding.
    if dir.len() + 1 + 3 + 40 + 17 > 100 {
        return None;
    }
    std::fs::create_dir_all(&base).ok()?;
    std::fs::set_permissions(&base, std::os::unix::fs::PermissionsExt::from_mode(0o700)).ok()?;
    Some(format!("{dir}/cm-%C"))
}

/// Spawn ssh to `remote`, skip whatever its shell printed, and do the handshake.
///
/// `batch` is for a connection nobody is watching: it never prompts, and ssh's complaint
/// is kept for the offline row instead of being written over the interface.
pub async fn open(remote: &Remote, batch: bool) -> io::Result<Link> {
    let mut transport = SshTransport::new(&remote.target);
    if let Some(path) = control_path() {
        transport = transport.with_control_path(path);
    }
    if batch {
        transport = transport.batch();
    }
    let argv = transport.argv(&remote_command(&remote.session, "relay"));
    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(if batch {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::inherit()
        })
        .kill_on_drop(true)
        .spawn()?;
    let said = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let said = said.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if !line.is_empty()
                    && let Ok(mut said) = said.lock()
                {
                    *said = line.to_string();
                }
            }
        });
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("ssh has no stdout"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("ssh has no stdin"))?;
    let connected = tokio::time::timeout(OPEN_DEADLINE, async {
        let mut out = BufReader::new(stdout);
        skip_to_marker(&mut out).await?;
        client::handshake(tokio::io::join(out, stdin), "qoro-tui").await
    })
    .await;
    let failure = |e: io::Error| {
        // Give ssh's own words a moment to arrive: they are the reason, and the error from
        // the pipe is only that the pipe closed.
        let said = said.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let said = said.lock().map(|s| s.clone()).unwrap_or_default();
            if said.is_empty() {
                e
            } else {
                io::Error::new(e.kind(), said)
            }
        }
    };
    match connected {
        Ok(Ok((stream, leftover))) => Ok(Link {
            stream,
            leftover,
            child,
            said,
        }),
        Ok(Err(e)) => Err(failure(e).await),
        Err(_) => Err(failure(io::Error::new(
            io::ErrorKind::TimedOut,
            "the remote did not answer in time",
        ))
        .await),
    }
}

/// What a machine said when asked about its sessions.
#[derive(Debug)]
pub enum Probe {
    Sessions(Vec<ListedSession>),
    /// No qoro there, on a machine qoro is built for.
    Missing(Platform),
}

/// A machine qoro can be installed on, as its own `uname` and `id` describe it. Only the
/// fixed names come back: nothing the remote printed is ever shown as it was printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
    pub root: bool,
}

impl Platform {
    pub fn describe(&self) -> String {
        format!("{} {}", self.os, self.arch)
    }
}

/// The sessions on the machine at `target`, as its own `qoro ls` lists them, or that qoro
/// is not there.
///
/// Asked from inside the interface, where nobody can answer a prompt: it never asks, and
/// it takes a host key it has not seen before, since the person just typed that address.
/// What ssh says when it cannot get in is the error, word for word.
pub async fn list_sessions(target: &str) -> io::Result<Probe> {
    let hello = run(target, &remote_command("default", "--hello"), None).await?;
    // Told apart by what happened, not by what was said: the remote command prints the
    // marker and exits 127 when it finds no qoro, and nothing else does both.
    if hello.marked.is_some() && hello.status == Some(127) {
        return Ok(Probe::Missing(platform_of(target).await?));
    }
    // Its codec first: a machine this build cannot talk to is said so now, rather than
    // added and then shown offline for good.
    let hello: qoro_remote::RemoteHello = serde_json::from_str(&hello.answer()?)
        .map_err(|_| io::Error::other(format!("{target} answered something that is not qoro")))?;
    qoro_remote::compatibility(&hello).map_err(|e| io::Error::other(e.to_string()))?;
    let listed = run(target, &remote_command("default", "ls"), None).await?;
    Ok(Probe::Sessions(parse_sessions(&listed.answer()?)))
}

/// Which platform the machine at `target` is, refusing one no build of qoro runs on.
async fn platform_of(target: &str) -> io::Result<Platform> {
    let script = format!(
        "printf '%s\\n' {RELAY_MARKER}; uname -s; uname -m; id -u; \
         if ldd --version 2>&1 | grep -qi musl; then echo musl; else echo other; fi"
    );
    let answer = run(target, &format!("sh -c {}", shell_quote(&script)), None)
        .await?
        .answer()?;
    let lines: Vec<&str> = answer.lines().map(str::trim).collect();
    let [system, machine, uid, libc] = lines.as_slice() else {
        return Err(io::Error::other("the machine did not say what it is"));
    };
    let Some((os, arch)) = qoro_runtime::release::platform(system, machine) else {
        return Err(io::Error::other(
            "the machine is not one qoro is built for; build it from source there",
        ));
    };
    if *libc == "musl" {
        return Err(io::Error::other(
            "the machine uses musl, which the qoro builds do not run on; build it from source there",
        ));
    }
    Ok(Platform {
        os,
        arch,
        root: *uid == "0",
    })
}

/// Put this version of qoro into `~/.local/bin` on the machine at `target`, and say which
/// version is there now.
///
/// In two steps over the same connection. The first writes the binary under a temporary
/// name, checks every byte arrived, and runs it; only an answer this build calls
/// compatible gets the second step, which links it into place. Anything short of that
/// leaves no `qoro` behind, because a broken or foreign one there would be found by every
/// later connection and never replaced: an existing qoro is never overwritten.
pub async fn install(target: &str) -> io::Result<String> {
    let platform = platform_of(target).await?;
    let binary = binary_for(platform).await?;
    let staged = run(
        target,
        &format!(
            "sh -c {} qoro {}",
            shell_quote(&STAGE.replace("MARKER", RELAY_MARKER)),
            shell_quote(&binary.len().to_string())
        ),
        Some(binary),
    )
    .await?
    .answer()?;
    let (path, hello) = staged
        .split_once('\n')
        .ok_or_else(|| io::Error::other("the machine did not confirm the copy"))?;
    let path = path.trim().to_string();
    let hello: Option<qoro_remote::RemoteHello> = serde_json::from_str(hello.trim()).ok();
    let fits = hello
        .as_ref()
        .map(qoro_remote::compatibility)
        .is_some_and(|c| c == Ok(qoro_remote::Compatibility::AsIs));
    if !fits {
        let _ = run(target, &guarded(ABORT, &path), None).await;
        return Err(io::Error::other(
            "the copied qoro does not speak this one's codec; nothing was installed",
        ));
    }
    run(target, &guarded(COMMIT, &path), None).await?.answer()?;
    let version: String = hello
        .map(|h| h.version)
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .take(32)
        .collect();
    Ok(version)
}

/// Stage: write stdin to a fresh temporary name in `~/.local/bin`, check its size against
/// `$1`, run it, and print the name and its `--hello`. A stale temporary from an interface
/// that died between the two steps is swept first.
const STAGE: &str = r#"set -eu
trap 'exit 1' HUP INT TERM PIPE
umask 022
d="$HOME/.local/bin"
mkdir -p "$d"
[ -O "$d" ] || { echo "$d belongs to another user" >&2; exit 3; }
find "$d" -maxdepth 1 -name '.qoro-install.*' -mmin +30 -exec rm -f {} + 2>/dev/null || true
if [ -L "$d/qoro" ] && [ ! -e "$d/qoro" ]; then echo "$d/qoro is a link to nothing; remove it" >&2; exit 4; fi
if [ -e "$d/qoro" ]; then echo "there is already a qoro in $d" >&2; exit 4; fi
t=$(mktemp "$d/.qoro-install.XXXXXX")
trap 'rm -f "$t"' EXIT
cat > "$t"
[ "$(wc -c < "$t" | tr -d ' ')" = "$1" ] || { echo "the copy arrived incomplete" >&2; exit 5; }
chmod 755 "$t"
printf '%s\n' MARKER "$t"
"$t" --hello
trap - EXIT
"#;

/// Commit: link the staged file into place, unless a qoro appeared there meanwhile, which
/// counts as done.
const COMMIT: &str = r#"ln "$1" "$d/qoro" 2>/dev/null || [ -e "$d/qoro" ]
rm -f "$1"
printf '%s\n' MARKER
"#;

/// Abort: take the staged file away.
const ABORT: &str = r#"rm -f "$1"
"#;

/// A step on a staged file, refusing any path that is not one of the stage's own names.
fn guarded(step: &str, path: &str) -> String {
    let step = step.replace("MARKER", RELAY_MARKER);
    let script = format!(
        "set -eu\nd=\"$HOME/.local/bin\"\n\
         case $1 in \"$d\"/.qoro-install.*) ;; *) echo 'not a staged install' >&2; exit 2;; esac\n\
         case ${{1#\"$d\"/.qoro-install.}} in ''|*/*) echo 'not a staged install' >&2; exit 2;; esac\n{step}"
    );
    format!("sh -c {} qoro {}", shell_quote(&script), shell_quote(path))
}

/// The binary to send: this one when it is the same platform and it is macOS, where one
/// build runs on every machine of that arch; otherwise the release of this version for that
/// platform, and this one only when no such release exists and the platforms match.
async fn binary_for(p: Platform) -> io::Result<Vec<u8>> {
    let same = (p.os, p.arch) == (std::env::consts::OS, std::env::consts::ARCH);
    if same && p.os == "macos" {
        return running_binary();
    }
    let asset = qoro_runtime::release::asset_for(p.os, p.arch)
        .ok_or_else(|| io::Error::other("no qoro is built for that machine"))?;
    let version = env!("CARGO_PKG_VERSION");
    let release =
        tokio::task::spawn_blocking(move || qoro_runtime::release::fetch_tagged(version, asset))
            .await
            .map_err(io::Error::other)?;
    let release = match release {
        Ok(release) => release,
        // No release of this version for it, which a build from source is: this binary is
        // the only one that fits, and the platform matches.
        Err(_) if same => return running_binary(),
        Err(e) => return Err(io::Error::other(e)),
    };
    // A release that exists but fails its checksum never falls back to anything.
    tokio::task::spawn_blocking(move || qoro_runtime::release::fetch_binary(&release))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)
}

/// This binary's own bytes. On Linux through `/proc/self/exe`, which still reads the
/// running file after it was replaced on disk; elsewhere the file the path resolves to.
fn running_binary() -> io::Result<Vec<u8>> {
    if cfg!(target_os = "linux") {
        std::fs::read("/proc/self/exe")
    } else {
        std::fs::read(std::fs::canonicalize(std::env::current_exe()?)?)
    }
}

/// What one remote command did: its exit status, what it printed after the marker if it
/// got that far, and the last thing it said on stderr, as plain text.
struct Answer {
    status: Option<i32>,
    marked: Option<String>,
    said: String,
}

impl Answer {
    /// What came after the marker, when the command also succeeded.
    fn answer(self) -> io::Result<String> {
        let said = |otherwise: &str| {
            io::Error::other(if self.said.is_empty() {
                otherwise.to_string()
            } else {
                self.said.clone()
            })
        };
        match (&self.marked, self.status) {
            (None, _) => Err(said("ssh failed")),
            (Some(_), Some(0)) => Ok(self.marked.clone().unwrap_or_default()),
            (Some(_), _) => Err(said("qoro there did not answer")),
        }
    }
}

/// Run `command` on the machine at `target`, with `input` on its stdin when given.
///
/// Output is read to a bound, not to the end: the other end is a machine the person only
/// just typed the address of, and a shell that prints forever would otherwise be held
/// here whole. The deadline grows with the input, at a floor of 32 KiB a second, and each
/// block of input must be taken within 30 seconds, so a stalled link fails without a slow
/// one being cut off.
async fn run(target: &str, command: &str, input: Option<Vec<u8>>) -> io::Result<Answer> {
    let mut transport = SshTransport::new(target).batch().accept_new_host();
    if let Some(path) = control_path() {
        transport = transport.with_control_path(path);
    }
    let argv = transport.argv(command);
    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let out = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("ssh has no stdout"))?;
    let err = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("ssh has no stderr"))?;
    let stdin = child.stdin.take();
    let size = input.as_ref().map_or(0, Vec::len) as u64;
    let deadline = OPEN_DEADLINE + Duration::from_secs(size / (32 * 1024));
    let limit = MARKER_LIMIT as u64;
    let finished = tokio::time::timeout(deadline, async {
        let feed = async {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                for block in input.chunks(64 * 1024) {
                    tokio::time::timeout(Duration::from_secs(30), stdin.write_all(block))
                        .await
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::TimedOut, "the copy stalled")
                        })??;
                }
                // Dropped here: the end of input is what tells the remote the copy is whole.
            }
            Ok::<(), io::Error>(())
        };
        let (mut out, mut err) = (out.take(limit), err.take(limit));
        let (fed, a, b) = tokio::join!(
            feed,
            out.read_to_end(&mut stdout),
            err.read_to_end(&mut stderr)
        );
        // Closed before waiting: a remote still printing past the bound gets a broken pipe
        // now rather than a full one it would sit on until the deadline.
        drop((out, err));
        a?;
        b?;
        let status = child.wait().await?;
        // A copy cut short is reported by the remote's own words when it said any.
        if let Err(e) = fed
            && status.success()
        {
            return Err(e);
        }
        Ok(status)
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the machine did not answer in time",
        )
    })??;
    // What ssh or the remote said, as one plain line: it is shown on the status line, and
    // an escape in it would reach this terminal as a command.
    let said: String = String::from_utf8_lossy(&stderr)
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .chars()
        // Control characters, and the invisible ones that reorder or hide text, which could
        // make a refusal read as something else.
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(200)
        .collect();
    let text = String::from_utf8_lossy(&stdout);
    Ok(Answer {
        status: finished.code(),
        marked: text
            .split_once(RELAY_MARKER)
            .map(|(_, after)| after.trim().to_string()),
        said: said.trim().to_string(),
    })
}

/// Read up to and including the marker line, keeping every byte after it buffered.
///
/// A line is compared whole after trimming, so a banner that happens to contain the marker
/// text inside a longer line is still a banner.
async fn skip_to_marker<R>(out: &mut BufReader<R>) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut seen = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut *out)
            .take((MARKER_LIMIT - seen) as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the remote closed the connection before qoro started",
            ));
        }
        seen += n;
        if line.trim_ascii() == RELAY_MARKER.as_bytes() {
            return Ok(());
        }
        if seen > MARKER_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the remote shell printed too much before qoro started",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn skip(input: &'static [u8]) -> (io::Result<()>, Vec<u8>) {
        let mut out = BufReader::new(input);
        let result = skip_to_marker(&mut out).await;
        let mut rest = Vec::new();
        let _ = out.read_to_end(&mut rest).await;
        (result, rest)
    }

    /// A login banner, then the marker, then binary protocol bytes: the banner goes, and
    /// every byte after the marker survives, including the ones already buffered.
    #[tokio::test]
    async fn a_banner_is_skipped_and_the_protocol_after_it_is_kept() {
        let (result, rest) =
            skip(b"Welcome to devbox\nlast login: never\n-----qoro-relay-----\n\x00\x01binary")
                .await;
        result.expect("marker found");
        assert_eq!(rest, b"\x00\x01binary");
    }

    #[tokio::test]
    async fn a_marker_inside_a_longer_line_is_not_the_marker() {
        let (result, _) = skip(b"echo -----qoro-relay----- later\n").await;
        assert_eq!(
            result.expect_err("eof").kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn a_shell_that_never_starts_qoro_is_cut_off() {
        let flood: &'static [u8] = Box::leak(vec![b'x'; MARKER_LIMIT * 2].into_boxed_slice());
        let (result, _) = skip(flood).await;
        assert_eq!(
            result.expect_err("too much").kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// Run one of the install steps the way ssh would, through a real `sh`, in a home of
    /// its own.
    fn step(home: &std::path::Path, command: &str, input: &[u8]) -> std::process::Output {
        use std::io::Write;
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env("HOME", home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("sh");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input)
            .expect("write");
        child.wait_with_output().expect("output")
    }

    fn stage(home: &std::path::Path, binary: &[u8], claimed: usize) -> std::process::Output {
        let command = format!(
            "sh -c {} qoro {}",
            shell_quote(&STAGE.replace("MARKER", RELAY_MARKER)),
            shell_quote(&claimed.to_string())
        );
        step(home, &command, binary)
    }

    const FAKE: &[u8] =
        b"#!/bin/sh\necho '{\"version\":\"9.9.9\",\"codec_generation\":1,\"platform\":\"test\"}'\n";

    fn leftovers(home: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(home.join(".local/bin"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The whole install on a real shell: stage writes and runs the copy under a temporary
    /// name and says where; commit links it into place and leaves nothing else behind.
    #[test]
    fn a_staged_copy_is_run_then_linked_into_place() {
        let home = tempfile::tempdir().expect("home");
        let out = stage(home.path(), FAKE, FAKE.len());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let after = text.split_once(RELAY_MARKER).expect("marker").1.trim();
        let (path, hello) = after.split_once('\n').expect("path then hello");
        assert!(path.contains("/.local/bin/.qoro-install."), "{path}");
        assert!(hello.contains("\"version\":\"9.9.9\""), "{hello}");
        assert!(
            !home.path().join(".local/bin/qoro").exists(),
            "linked before commit"
        );

        let out = step(home.path(), &guarded(COMMIT, path), b"");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read(home.path().join(".local/bin/qoro")).expect("qoro"),
            FAKE
        );
        assert_eq!(leftovers(home.path()), vec!["qoro".to_string()]);
    }

    /// A copy that arrived short is never run and never left behind.
    #[test]
    fn a_copy_cut_short_leaves_nothing() {
        let home = tempfile::tempdir().expect("home");
        let out = stage(home.path(), &FAKE[..10], FAKE.len());
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("incomplete"));
        assert!(
            leftovers(home.path()).is_empty(),
            "{:?}",
            leftovers(home.path())
        );
    }

    /// A binary that does not run there is refused before anything is linked.
    #[test]
    fn a_copy_that_does_not_run_leaves_nothing() {
        let home = tempfile::tempdir().expect("home");
        let broken = b"#!/bin/sh\nexit 1\n";
        let out = stage(home.path(), broken, broken.len());
        assert!(!out.status.success());
        assert!(
            leftovers(home.path()).is_empty(),
            "{:?}",
            leftovers(home.path())
        );
    }

    /// An existing qoro is never replaced, and a link to nothing is named for what it is.
    #[test]
    fn an_existing_qoro_is_never_touched() {
        let home = tempfile::tempdir().expect("home");
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join("qoro"), b"theirs").expect("theirs");
        let out = stage(home.path(), FAKE, FAKE.len());
        assert!(!out.status.success());
        assert_eq!(std::fs::read(bin.join("qoro")).expect("qoro"), b"theirs");

        std::fs::remove_file(bin.join("qoro")).expect("rm");
        std::os::unix::fs::symlink("/nonexistent/qoro", bin.join("qoro")).expect("link");
        let out = stage(home.path(), FAKE, FAKE.len());
        assert!(String::from_utf8_lossy(&out.stderr).contains("link to nothing"));
    }

    /// Commit and abort act only on a stage's own names, and a qoro that appeared between
    /// the two steps is left alone and counted as done.
    #[test]
    fn commit_touches_only_its_own_staged_file() {
        let home = tempfile::tempdir().expect("home");
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let victim = home.path().join("keep");
        std::fs::write(&victim, b"keep").expect("keep");
        let sneaky = format!("{}/.qoro-install.x/../../keep", bin.display());
        // A bare name has no prefix to strip, and ssh runs in the home, where `keep` is.
        for path in [sneaky.as_str(), "keep"] {
            for script in [COMMIT, ABORT] {
                let out = step(home.path(), &guarded(script, path), b"");
                assert!(!out.status.success(), "{path}");
            }
        }
        assert!(
            victim.exists(),
            "a path outside the stage's names was removed"
        );

        let staged = bin.join(".qoro-install.abc123");
        std::fs::write(&staged, FAKE).expect("staged");
        std::fs::write(bin.join("qoro"), b"arrived meanwhile").expect("theirs");
        let out = step(
            home.path(),
            &guarded(COMMIT, &staged.to_string_lossy()),
            b"",
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read(bin.join("qoro")).expect("qoro"),
            b"arrived meanwhile"
        );
        assert!(!staged.exists());
    }
}

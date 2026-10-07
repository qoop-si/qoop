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
use qoro_remote::saved::{MARKER_LIMIT, RELAY_MARKER, Remote, remote_command, shell_quote};
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
    /// The version the daemon over there is running, from its welcome. Not the qoro on its
    /// disk, which a push can have replaced while this daemon keeps running the old file.
    pub server_version: String,
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
/// `key` is the link's own, the same on every reconnect, so the far daemon ends the
/// connection this one replaces.
pub async fn open(remote: &Remote, batch: bool, key: &str) -> io::Result<Link> {
    let mut transport = SshTransport::new(&remote.target);
    if let Some(path) = control_path() {
        transport = transport.with_control_path(path);
    }
    if batch {
        transport = transport.batch();
    }
    let argv = transport.argv(&remote_command("relay"));
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
                let line = plain(line.trim());
                if !line.is_empty()
                    && let Ok(mut said) = said.lock()
                {
                    *said = line;
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
        client::handshake(tokio::io::join(out, stdin), "qoro-tui", Some(key)).await
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
        Ok(Ok((stream, leftover, welcome))) => Ok(Link {
            stream,
            leftover,
            child,
            said,
            server_version: welcome.server_version,
        }),
        Ok(Err(e)) => Err(failure(e).await),
        Err(_) => Err(failure(io::Error::new(
            io::ErrorKind::TimedOut,
            "the remote did not answer in time",
        ))
        .await),
    }
}

/// What a machine said when asked whether qoro is there.
#[derive(Debug)]
pub enum Probe {
    /// A qoro this build can talk to, and the version the binary on its disk says it is.
    Ready { version: String },
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

/// Whether the machine at `target` has a qoro this build can talk to, and which version
/// that binary says it is; or which machine it is when it has none.
///
/// Asked from inside the interface, where nobody can answer a prompt: it never asks, and
/// it takes a host key it has not seen before, since the person just typed that address.
/// What ssh says when it cannot get in is the error, word for word.
pub async fn probe(target: &str) -> io::Result<Probe> {
    let hello = run(target, &remote_command("--hello"), None).await?;
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
    Ok(Probe::Ready {
        version: hello.version,
    })
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

/// What putting this version of qoro on a machine came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// The version the qoro on its disk says it is now, which its daemon is asked to move
    /// to: this one after a swap, or whatever was there when it was already as new. The
    /// second is what went wrong refreshing its hooks and skills there, when anything did.
    OnDisk(String, Option<String>),
    /// No release of this version could be read, which a build from source between
    /// releases is: nothing was sent, and there is nothing to tell anybody.
    NoRelease,
}

/// Put the release of `version` in place of the older qoro at `target`, for its daemon
/// to move onto when nothing there is working. The interface asks for the version it is;
/// `qoro update` for the one it has just installed.
///
/// Only a tagged release goes, never the running binary: a same-platform Mac would
/// otherwise be handed whatever was built here last. The qoro replaced is the one
/// `remote_command` runs, swapped atomically beside itself, and only while it still says it
/// is the version this asked about; one that changed meanwhile (another window or another
/// Mac swapped first) counts as done. A qoro there as new as this one, or newer, is left
/// alone: nothing is ever moved backwards.
pub async fn update(target: &str, version: &str) -> io::Result<Update> {
    let Probe::Ready { version: disk } = probe(target).await? else {
        return Err(io::Error::other("no qoro is there any more"));
    };
    if !qoro_runtime::paths::is_newer_version(version, &disk) {
        return Ok(Update::OnDisk(disk, None));
    }
    let platform = platform_of(target).await?;
    let asset = qoro_runtime::release::asset_for(platform.os, platform.arch)
        .ok_or_else(|| io::Error::other("no qoro is built for that machine"))?;
    let tag = version.to_string();
    let release =
        tokio::task::spawn_blocking(move || qoro_runtime::release::fetch_tagged(&tag, asset))
            .await
            .map_err(io::Error::other)?;
    let release = match release {
        Ok(release) => release,
        Err(e) => {
            let github = std::env::var(qoro_runtime::release::RELEASE_BASE_ENV)
                .unwrap_or_default()
                .is_empty();
            let anonymous = github
                && tokio::task::spawn_blocking(qoro_runtime::release::github_token)
                    .await
                    .map_err(io::Error::other)?
                    .is_none();
            return no_release(e, anonymous);
        }
    };
    // A release that is there but fails its checksum, or has none, is said: it is not the
    // quiet case of a build that has no release at all.
    let binary = tokio::task::spawn_blocking(move || qoro_runtime::release::fetch_binary(&release))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    let swapped = run(
        target,
        &swap_command(&disk, version, binary.len()),
        Some(binary),
    )
    .await?;
    // Another window or another Mac swapped first, maybe to another version than this one:
    // what is on disk now is asked again, and that is what its daemon moves to.
    if swapped.status == Some(SWAPPED_MEANWHILE) {
        let Probe::Ready { version } = probe(target).await? else {
            return Err(io::Error::other("no qoro is there any more"));
        };
        return Ok(Update::OnDisk(version, None));
    }
    // Lines the remote printed after the marker: plain, and as one line each of its own.
    let warning = swapped
        .answer()?
        .lines()
        .map(plain)
        .collect::<Vec<_>>()
        .join("; ");
    Ok(Update::OnDisk(
        version.to_string(),
        (!warning.is_empty()).then_some(warning),
    ))
}

/// What a release of this version that could not be read comes to. Only a release that
/// is not there is the quiet case: the tag answers 404, or the release has no binary for
/// that machine. Anything else (no network, a limit reached, an answer that is not the
/// API's) is said, and so is a 404 asked without a token, since the releases are private
/// and without one nothing is found at all.
fn no_release(error: String, anonymous: bool) -> io::Result<Update> {
    if error.contains("the release carries no") {
        return Ok(Update::NoRelease);
    }
    if error.contains("returned error: 404") {
        if anonymous {
            return Err(io::Error::other(format!(
                "{error}; the releases are private, so this Mac needs a GitHub token: run \
                 `gh auth login` or set GH_TOKEN"
            )));
        }
        return Ok(Update::NoRelease);
    }
    Err(io::Error::other(error))
}

/// The exit status `SWAP` gives when the qoro there is no longer the one asked about.
const SWAPPED_MEANWHILE: i32 = 6;

/// Swap: find the qoro `remote_command` runs, as the real file behind any link; stage stdin
/// beside it, so the rename is on one filesystem; check the size against `$3` and that the
/// copy says it is `$2`; then rename it over the original, but only while the original
/// still says it is `$1`. The check and the rename hold a lock beside the file, a
/// directory made or not in one step, so two swaps never both pass the check. Exits 3
/// when the place is not this user's to write, 4 when the file there is not qoro at all,
/// 5 when the copy is wrong, 6 only when another version of qoro is there now, 7 when
/// another swap holds the lock (said, not quiet: a lock left by a swap that was killed
/// would otherwise hide every update), and leaves nothing staged and no lock of its own
/// behind. A lock older than half an hour is from a swap that was killed, and is swept.
/// Once swapped, the new qoro refreshes its hooks and skills there (it skips an agent that
/// is not on that machine); what fails is said after the marker and never fails the swap.
/// It also puts the bridge service there back on the new file: launchd refuses to start a
/// service whose program file was replaced, so a macOS machine's bridge would otherwise die
/// at its next start. That reload gets `BRIDGE_WAIT` seconds, kept by a watcher since not
/// every machine has `timeout`, and a reload that fails or runs out is said like the others.
/// `qoro update` waits out exit 7 by matching its sentence ("update of qoro is in progress"),
/// so the two are reworded together.
const SWAP: &str = r#"set -eu
trap 'exit 1' HUP INT TERM PIPE
umask 022
r=
for q in qoro "$HOME/.local/bin/qoro" "$HOME/.cargo/bin/qoro"; do
  p=$(command -v "$q") && { r=$(readlink -f "$p") || { echo "cannot resolve $p" >&2; exit 3; }; break; }
done
[ -n "$r" ] && [ -f "$r" ] || { echo "no qoro file to replace" >&2; exit 3; }
d=$(dirname "$r")
[ -O "$d" ] && [ -w "$d" ] || { echo "$d is not this user's to write" >&2; exit 3; }
l="$d/.qoro-swap.lock"
find "$d" -maxdepth 1 -name .qoro-swap.lock -mmin +30 -exec rmdir {} + 2>/dev/null || true
t=
h=
trap '[ -z "$t" ] || rm -f "$t"; [ -z "$h" ] || rmdir "$l"' EXIT
mkdir "$l" 2>/dev/null || { echo "another update of qoro is in progress there; try again later" >&2; exit 7; }
h=1
v=$("$r" --version 2>/dev/null) || v=
case $v in "qoro $1") ;; "qoro "*) echo "qoro changed meanwhile" >&2; exit 6;; *) echo "$r is not qoro" >&2; exit 4;; esac
t=$(mktemp "$d/.qoro-install.XXXXXX")
cat > "$t"
[ "$(wc -c < "$t" | tr -d ' ')" = "$3" ] || { echo "the copy arrived incomplete" >&2; exit 5; }
chmod 755 "$t"
[ "$("$t" --version 2>/dev/null)" = "qoro $2" ] || { echo "the copy is not qoro $2" >&2; exit 5; }
mv -f "$t" "$r"
t=
printf '%s\n' MARKER
for w in hooks skills; do
  "$r" "$w" install </dev/null >/dev/null 2>&1 || echo "the $w there were not refreshed: run qoro $w install on it"
done
"$r" bridge reload </dev/null >/dev/null 2>&1 &
b=$!
( sleep BRIDGE_WAIT; kill "$b" ) >/dev/null 2>&1 &
k=$!
wait "$b" || echo "the bridge there was not reloaded: run qoro bridge reload on it"
kill "$k" 2>/dev/null || true
"#;

/// How long the bridge reload at the end of `SWAP` may take. A second in tests, so one that
/// hangs can be seen to be cut off.
const BRIDGE_WAIT: u32 = if cfg!(test) { 1 } else { 15 };

/// The ssh command for `SWAP`, from the version on disk to `new`, `size` bytes on stdin.
fn swap_command(disk: &str, new: &str, size: usize) -> String {
    format!(
        "sh -c {} qoro {} {} {}",
        shell_quote(
            &SWAP
                .replace("MARKER", RELAY_MARKER)
                .replace("BRIDGE_WAIT", &BRIDGE_WAIT.to_string())
        ),
        shell_quote(disk),
        shell_quote(new),
        shell_quote(&size.to_string())
    )
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

/// Something a remote said, as one plain line of at most 200 characters.
///
/// Control characters go, and so do the invisible ones that reorder or hide text, which
/// could make a refusal read as something else: what a remote prints reaches a terminal
/// as a command otherwise. Everything of a remote's that is shown goes through this.
pub fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(200)
        .collect()
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
    let said = plain(
        String::from_utf8_lossy(&stderr)
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or_default(),
    );
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

    /// What a remote says is shown as plain text: no escape, no hidden reordering, bounded.
    #[test]
    fn what_a_remote_says_is_plain() {
        assert_eq!(plain("a\u{1b}[31mb\u{202E}c\u{7}"), "a[31mbc");
        assert_eq!(plain(&"x".repeat(500)).len(), 200);
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

    /// A qoro that says it is `version`, as a script.
    fn says(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho 'qoro {version}'\n").into_bytes()
    }

    /// Run `SWAP` from `disk` to `new` with `binary` on stdin, in a home of its own and on a
    /// PATH with no qoro on it: the one found is the home's, never one of this machine's.
    fn swap(home: &std::path::Path, disk: &str, new: &str, binary: &[u8]) -> std::process::Output {
        use std::io::Write;
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(swap_command(disk, new, binary.len()))
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("sh");
        // A refusal before the copy closes stdin early: the broken pipe is expected.
        let _ = child.stdin.take().expect("stdin").write_all(binary);
        child.wait_with_output().expect("output")
    }

    fn installed(home: &std::path::Path, what: &[u8]) -> std::path::PathBuf {
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let qoro = bin.join("qoro");
        std::fs::write(&qoro, what).expect("qoro");
        std::fs::set_permissions(&qoro, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        qoro
    }

    /// An older qoro is replaced by the new one, in place, with nothing else left there.
    #[test]
    fn swap_replaces_an_older_qoro() {
        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        let out = swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains(RELAY_MARKER));
        assert_eq!(std::fs::read(&qoro).expect("qoro"), says("0.0.2"));
        assert_eq!(leftovers(home.path()), vec!["qoro".to_string()]);
    }

    /// A qoro that says it is `version`, notes each `hooks`, `skills` or `bridge` call in
    /// `$HOME/ran`, fails `skills` when `$HOME/skills-fail` exists, and `bridge` when
    /// `$HOME/bridge-fail` does or hangs it when `$HOME/bridge-hang` does.
    fn noting(version: &str) -> Vec<u8> {
        format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo 'qoro {version}';;\n\
             hooks|skills|bridge) echo \"$*\" >> \"$HOME/ran\"; \
             [ \"$1\" = skills ] && [ -e \"$HOME/skills-fail\" ] && exit 1; \
             [ \"$1\" = bridge ] && [ -e \"$HOME/bridge-hang\" ] && exec sleep 30; \
             [ \"$1\" = bridge ] && [ -e \"$HOME/bridge-fail\" ] && exit 1; exit 0;;\nesac\n"
        )
        .into_bytes()
    }

    /// The qoro just put there refreshes its hooks and skills and reloads its bridge, and a
    /// failure of any is said after the marker without failing the swap.
    #[test]
    fn swap_refreshes_hooks_and_skills_and_says_what_failed() {
        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        let out = swap(home.path(), "0.0.1", "0.0.2", &noting("0.0.2"));
        assert!(out.status.success());
        let said = String::from_utf8_lossy(&out.stdout);
        assert_eq!(said.split_once(RELAY_MARKER).expect("marker").1.trim(), "");
        assert_eq!(
            std::fs::read_to_string(home.path().join("ran")).expect("ran"),
            "hooks install\nskills install\nbridge reload\n"
        );
        assert_eq!(std::fs::read(&qoro).expect("qoro"), noting("0.0.2"));

        installed(home.path(), &says("0.0.1"));
        std::fs::write(home.path().join("skills-fail"), "").expect("flag");
        std::fs::write(home.path().join("bridge-fail"), "").expect("flag");
        let out = swap(home.path(), "0.0.1", "0.0.2", &noting("0.0.2"));
        assert!(out.status.success(), "a refresh must never fail the swap");
        let said = String::from_utf8_lossy(&out.stdout);
        let after = said.split_once(RELAY_MARKER).expect("marker").1.trim();
        assert!(after.contains("skills there were not refreshed"), "{after}");
        assert!(after.contains("bridge there was not reloaded"), "{after}");
        assert!(!after.contains("hooks"), "{after}");
        assert_eq!(std::fs::read(&qoro).expect("qoro"), noting("0.0.2"));
    }

    /// A reload that hangs is cut off, said, and does not fail the swap or hold it.
    #[test]
    fn swap_cuts_off_a_bridge_reload_that_hangs() {
        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        std::fs::write(home.path().join("bridge-hang"), "").expect("flag");
        let started = std::time::Instant::now();
        let out = swap(home.path(), "0.0.1", "0.0.2", &noting("0.0.2"));
        assert!(out.status.success());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the reload was waited out"
        );
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains("bridge there was not reloaded"), "{said}");
        assert_eq!(std::fs::read(&qoro).expect("qoro"), noting("0.0.2"));
    }

    /// A qoro that is a link has the file behind it replaced, and stays a link.
    #[test]
    fn swap_follows_a_link_and_keeps_it_a_link() {
        let home = tempfile::tempdir().expect("home");
        let real = home.path().join("opt");
        std::fs::create_dir_all(&real).expect("opt");
        let target = real.join("qoro");
        std::fs::write(&target, says("0.0.1")).expect("qoro");
        std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::os::unix::fs::symlink(&target, bin.join("qoro")).expect("link");
        let out = swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            std::fs::symlink_metadata(bin.join("qoro"))
                .expect("link")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).expect("qoro"), says("0.0.2"));
        let left: Vec<_> = std::fs::read_dir(&real).expect("opt").flatten().collect();
        assert_eq!(left.len(), 1, "something was left staged beside the file");
    }

    /// Whatever it refuses, the file there is untouched and nothing is left staged: another
    /// program (an error, never read as a swap done elsewhere), a qoro of another version
    /// than asked about (which another swap put there meanwhile), a link to nothing, a place
    /// this user cannot write, and a copy that is not the version it claims.
    #[test]
    fn swap_refuses_and_leaves_nothing() {
        let check = |home: &std::path::Path, out: std::process::Output, code: i32| {
            assert_eq!(
                out.status.code(),
                Some(code),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(!String::from_utf8_lossy(&out.stdout).contains(RELAY_MARKER));
            let left = leftovers(home);
            assert!(
                left.iter().all(|name| !name.starts_with(".qoro-")),
                "{left:?}"
            );
        };

        let home = tempfile::tempdir().expect("home");
        let other = b"#!/bin/sh\necho hello\n";
        let qoro = installed(home.path(), other);
        check(
            home.path(),
            swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2")),
            4,
        );
        assert_eq!(std::fs::read(&qoro).expect("qoro"), other);

        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        check(
            home.path(),
            swap(home.path(), "0.0.0", "0.0.2", &says("0.0.2")),
            SWAPPED_MEANWHILE,
        );
        check(
            home.path(),
            swap(home.path(), "0.0.1", "0.0.2", &says("0.0.3")),
            5,
        );
        assert_eq!(std::fs::read(&qoro).expect("qoro"), says("0.0.1"));

        let home = tempfile::tempdir().expect("home");
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::os::unix::fs::symlink("/nonexistent/qoro", bin.join("qoro")).expect("link");
        check(
            home.path(),
            swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2")),
            3,
        );

        // Owned by another user cannot be made here without root; a directory this user
        // may not write is refused by the same line.
        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        let bin = home.path().join(".local/bin");
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o555))
            .expect("chmod");
        let out = swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2"));
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        check(home.path(), out, 3);
        assert_eq!(std::fs::read(&qoro).expect("qoro"), says("0.0.1"));

        // Another swap holds the lock: refused as one under way, and its lock is its own.
        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        let lock = home.path().join(".local/bin/.qoro-swap.lock");
        std::fs::create_dir(&lock).expect("lock");
        let out = swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2"));
        assert_eq!(out.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&out.stderr).contains("in progress"));
        assert!(lock.is_dir(), "another swap's lock was taken away");
        assert_eq!(std::fs::read(&qoro).expect("qoro"), says("0.0.1"));
        // One left by a swap killed long ago is swept, and the swap goes ahead.
        let touched = std::process::Command::new("touch")
            .args(["-t", "200001010000"])
            .arg(&lock)
            .status()
            .expect("touch");
        assert!(touched.success());
        let out = swap(home.path(), "0.0.1", "0.0.2", &says("0.0.2"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(std::fs::read(&qoro).expect("qoro"), says("0.0.2"));
        assert_eq!(leftovers(home.path()), vec!["qoro".to_string()]);
    }

    /// Only a release that is not there is the quiet case; a 404 asked without a token says
    /// how to get one, and every other failure is said as it is.
    #[test]
    fn only_a_missing_release_is_quiet() {
        let missing = "no release of qoro 9.9.9 could be read: curl: (22) The requested URL \
                       returned error: 404";
        assert_eq!(
            no_release(missing.into(), false).expect("quiet"),
            Update::NoRelease
        );
        assert_eq!(
            no_release("the release carries no qoro-x.tar.gz".into(), true).expect("quiet"),
            Update::NoRelease
        );
        let said = no_release(missing.into(), true)
            .expect_err("said")
            .to_string();
        assert!(
            said.contains("gh auth login") && said.contains("GH_TOKEN"),
            "{said}"
        );
        for other in [
            "no release of qoro 9.9.9 could be read: curl: (6) Could not resolve host",
            "no release of qoro 9.9.9 could be read: curl: (22) The requested URL returned \
             error: 403",
            "the releases API answered what is not JSON: expected value",
        ] {
            for anonymous in [false, true] {
                let said = no_release(other.into(), anonymous).expect_err("said");
                assert!(said.to_string().starts_with(other), "{said}");
            }
        }
    }

    /// Two swaps racing at one qoro leave one valid new binary: exactly one swaps, the other
    /// is refused as another swap in progress (7) or done (6), and nothing is left staged
    /// or locked.
    #[test]
    fn two_swaps_at_once_leave_one_valid_qoro() {
        let home = tempfile::tempdir().expect("home");
        let qoro = installed(home.path(), &says("0.0.1"));
        let path = home.path().to_path_buf();
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let home = path.clone();
                std::thread::spawn(move || swap(&home, "0.0.1", "0.0.2", &says("0.0.2")))
            })
            .collect();
        let mut codes: Vec<Option<i32>> = racers
            .into_iter()
            .map(|racer| racer.join().expect("racer").status.code())
            .collect();
        codes.sort();
        assert!(
            matches!(codes.as_slice(), [Some(0), Some(SWAPPED_MEANWHILE | 7)]),
            "{codes:?}"
        );
        assert_eq!(std::fs::read(&qoro).expect("qoro"), says("0.0.2"));
        assert_eq!(leftovers(home.path()), vec!["qoro".to_string()]);
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

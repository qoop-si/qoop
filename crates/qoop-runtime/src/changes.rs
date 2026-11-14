//! What an agent's checkout changed, for the owner to judge on the phone (`Changes`), and the
//! deadline runner every external command of the CLI and the daemon goes through.
//!
//! git runs here in a checkout an agent works in, and that agent may have written the
//! checkout's own `.git/config`. The daemon runs outside the agent's sandbox, so nothing that
//! config names may run: fsmonitor, hooks, every filter it names, diff drivers and the pager
//! are turned off in the command line scope (`GIT_CONFIG_COUNT`), which beats the
//! repository's config and takes any name, `[filter "a=b"]` included. A config that cannot be
//! read whole, a git too old for that scope, or a config that grew a filter since it was first
//! read, is refused: unread is unsafe. One window stays: the config is read again just before
//! the diff and before the status, and a filter added between that read and git's own start
//! of the command would run. Closing it needs a config git takes that the checkout cannot
//! change, and git has no option for one.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::asks::failed;
use crate::control::{ChangedFile, ControlResponse, PullRequest};
use crate::shows::github_repo;

/// The most bytes of any one command's output kept, the diff's included.
pub const CAP: usize = 256 * 1024;

/// All of one answer's git commands, then its `gh`.
const GIT_WITHIN: Duration = Duration::from_secs(5);
const GH_WITHIN: Duration = Duration::from_secs(5);

/// What a diff is taken from when there is no commit at all.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// What the repository's config cannot turn back on.
const OFF: [(&str, &str); 4] = [
    ("core.fsmonitor", "false"),
    ("core.hooksPath", "/dev/null"),
    ("diff.external", ""),
    ("core.pager", "cat"),
];

/// After `diff`: no driver, conversion, colour or submodule of the checkout's choosing.
const DIFF: [&str; 4] = [
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--ignore-submodules=all",
];

const UNREAD: &str = "the repository config could not be read";

/// What `cmd` printed, up to `cap` bytes (none: all of it) and whether there was more, when it
/// exits 0 by `by`; output past the cap is kept whatever the exit. The output is read on a
/// thread of its own, so a full pipe never stalls the command. On every way out the command's
/// process group is killed, everything it started with it, before its leader is reaped, so the
/// group's id is still its own.
pub fn run_by(cmd: &mut Command, by: Instant, cap: Option<usize>) -> Option<(Vec<u8>, bool)> {
    use std::io::Read as _;
    use std::os::unix::process::CommandExt as _;
    let cap = cap.unwrap_or(usize::MAX);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let Ok(pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    // Shared as it is read, so what came before a pipe something outside the group still holds
    // is kept.
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (done, finished) = std::sync::mpsc::channel();
    if let Some(mut stdout) = child.stdout.take() {
        let got = got.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                let mut got = got.lock().unwrap();
                got.extend_from_slice(&buf[..n]);
                if got.len() > cap {
                    break;
                }
            }
            let _ = done.send(());
        });
    }
    // Exited, but not reaped: `WNOWAIT` leaves the leader a zombie holding its id.
    let exited = || {
        // SAFETY: a zeroed siginfo is valid, and the call only writes it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        // SAFETY: asks about this call's own child, which is not reaped yet.
        let asked = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
        asked == 0 && info.si_signo == libc::SIGCHLD
    };
    let over = || got.lock().unwrap().len() > cap;
    while !over() && !exited() && Instant::now() < by {
        std::thread::sleep(Duration::from_millis(2));
    }
    // SAFETY: a plain signal to the process group this call made, whose leader is unreaped.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let status = child.wait().ok();
    // The group is gone and its pipe ends with it, unless something that left the group holds
    // it: then what was read by now is the output.
    let _ = finished.recv_timeout(Duration::from_secs(1));
    let mut out = std::mem::take(&mut *got.lock().unwrap());
    if out.len() > cap {
        out.truncate(cap);
        return Some((out, true));
    }
    status.filter(|s| s.success()).map(|_| (out, false))
}

/// git in one checkout with what turns its own commands off, by one deadline.
struct Git {
    root: PathBuf,
    /// The filters named when the config was read, each turned off.
    filters: Vec<String>,
    by: Instant,
}

impl Git {
    fn run(&self, args: &[&str]) -> Option<(Vec<u8>, bool)> {
        let mut set: Vec<(String, &str)> = OFF.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for name in &self.filters {
            for (key, value) in [
                ("clean", ""),
                ("smudge", ""),
                ("process", ""),
                ("required", "false"),
            ] {
                set.push((format!("filter.{name}.{key}"), value));
            }
        }
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.root).args(args);
        // The daemon's own environment points git nowhere else.
        for inherited in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
        ] {
            cmd.env_remove(inherited);
        }
        cmd.env("GIT_CONFIG_COUNT", set.len().to_string())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C");
        for (i, (key, value)) in set.iter().enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), key)
                .env(format!("GIT_CONFIG_VALUE_{i}"), value);
        }
        run_by(&mut cmd, self.by, Some(CAP))
    }

    /// One line of output, trimmed; `None` when the command failed or printed nothing.
    fn line(&self, args: &[&str]) -> Option<String> {
        let (out, _) = self.run(args)?;
        let line = String::from_utf8_lossy(&out).trim().to_string();
        (!line.is_empty()).then_some(line)
    }

    /// Every filter the config names in any scope, read whole or not at all (reading runs
    /// nothing).
    fn named_filters(&self) -> Result<Vec<String>, String> {
        match self.run(&["config", "--list", "-z"]) {
            Some((config, false)) => Ok(filters_in(&config)),
            _ => Err(UNREAD.into()),
        }
    }

    /// A command that may run filters, after the config is read again: a filter named since
    /// would run unturned.
    fn guarded(&self, args: &[&str]) -> Result<Option<(Vec<u8>, bool)>, String> {
        if self
            .named_filters()?
            .iter()
            .any(|name| !self.filters.contains(name))
        {
            return Err(UNREAD.into());
        }
        Ok(self.run(args))
    }
}

/// The names of the filters in a `git config --list -z`.
fn filters_in(config: &[u8]) -> Vec<String> {
    let mut names: Vec<String> = String::from_utf8_lossy(config)
        .split('\0')
        .filter_map(|entry| {
            let key = entry.split('\n').next()?.strip_prefix("filter.")?;
            ["clean", "smudge", "process", "required"]
                .iter()
                .find_map(|end| key.strip_suffix(end)?.strip_suffix('.'))
                .map(str::to_owned)
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The checkout `cwd` is in, its config read, every command of it done `within`.
fn open(cwd: &str, within: Duration) -> Result<Git, String> {
    let mut git = Git {
        root: PathBuf::from(cwd),
        filters: Vec::new(),
        by: Instant::now() + within,
    };
    // `GIT_CONFIG_COUNT` came in 2.31: an older git ignores it and would run what it turns off.
    let version = git.line(&["--version"]).unwrap_or_default();
    let mut numbers = version
        .strip_prefix("git version ")
        .unwrap_or("")
        .split(['.', ' '])
        .map(|n| n.parse::<u32>().unwrap_or(0));
    if (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0)) < (2, 31) {
        return Err(UNREAD.into());
    }
    let root = git
        .line(&["rev-parse", "--show-toplevel"])
        .ok_or("not a git repository")?;
    git.root = PathBuf::from(root);
    git.filters = git.named_filters()?;
    Ok(git)
}

/// The default branch: `origin/HEAD`'s, else `main`, else `HEAD` itself.
fn base_of(git: &Git) -> String {
    git.line(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .filter(|base| {
        let commit = format!("{base}^{{commit}}");
        !base.starts_with('-')
            && git
                .line(&["rev-parse", "--verify", "--quiet", &commit])
                .is_some()
    })
    .or_else(|| {
        git.line(&["rev-parse", "--verify", "--quiet", "main^{commit}"])
            .map(|_| "main".to_string())
    })
    .unwrap_or_else(|| "HEAD".to_string())
}

/// Where HEAD left `base`; HEAD when they never met, the empty tree with no commit at all.
fn from_of(git: &Git, base: &str) -> String {
    git.line(&["merge-base", "--end-of-options", "HEAD", base])
        .or_else(|| {
            git.line(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
                .map(|_| "HEAD".to_string())
        })
        .unwrap_or_else(|| EMPTY_TREE.to_string())
}

/// The files and the patch of one `git diff --patch-with-raw -z`: raw records
/// (`:<modes> <ids> <letter>`, the path, and for a rename or copy the new path, each ending in
/// NUL), one NUL, then the patch. A list cut short leaves no patch.
fn raw_and_patch(out: &[u8]) -> (Vec<ChangedFile>, &[u8]) {
    let mut files = Vec::new();
    let mut rest = out;
    let field = |rest: &mut &[u8]| {
        let at = rest.iter().position(|&b| b == 0)?;
        let text = String::from_utf8_lossy(&rest[..at]).into_owned();
        *rest = &rest[at + 1..];
        Some(text)
    };
    while rest.first() == Some(&b':') {
        let (Some(head), Some(mut path)) = (field(&mut rest), field(&mut rest)) else {
            return (files, &[]);
        };
        let status = head.rsplit(' ').next().unwrap_or("");
        if status.starts_with(['R', 'C']) {
            let Some(new) = field(&mut rest) else {
                return (files, &[]);
            };
            path = new;
        }
        let status = status.get(..1).unwrap_or("").to_string();
        files.push(ChangedFile { path, status });
    }
    (files, rest.strip_prefix(&[0]).unwrap_or(rest))
}

/// The files changed since `from` and the patch, cut on a line's end at the cap.
fn diff_of(git: &Git, from: &str) -> Result<(Vec<ChangedFile>, String, bool), String> {
    let args = [
        &["diff", "--patch-with-raw", "-z"][..],
        &DIFF,
        &[from, "--"],
    ]
    .concat();
    let (out, cut) = git.guarded(&args)?.ok_or("git diff did not answer")?;
    let (files, mut patch) = raw_and_patch(&out);
    if cut {
        // So the last line shown is a whole one.
        patch = &patch[..patch.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)];
    }
    Ok((files, String::from_utf8_lossy(patch).into_owned(), cut))
}

/// The untracked files, whether the list was cut, and whether git answered at all. Unanswered
/// is none: a checkout full of them is slow here, and loses only this list.
fn untracked_of(git: &Git) -> Result<(Vec<ChangedFile>, bool, bool), String> {
    let args = [
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=all",
    ];
    let Some((out, cut)) = git.guarded(&args)? else {
        return Ok((Vec::new(), false, false));
    };
    let mut files = Vec::new();
    let text = String::from_utf8_lossy(&out);
    let mut entries: Vec<&str> = text.split('\0').collect();
    // The last is empty, or was cut.
    entries.pop();
    let mut entries = entries.into_iter();
    while let Some(entry) = entries.next() {
        if entry.starts_with(['R', 'C']) {
            entries.next();
        } else if let Some(path) = entry.strip_prefix("?? ") {
            let (path, status) = (path.to_string(), "?".to_string());
            files.push(ChangedFile { path, status });
        }
    }
    Ok((files, cut, true))
}

/// How much a checkout changed since a base: files, lines added and removed, and whether a
/// count may be low (git's answer was cut, or the untracked files were not listed in time).
#[derive(Debug, Default, PartialEq)]
pub struct Stat {
    pub files: usize,
    pub added: u64,
    pub removed: u64,
    pub cut: bool,
}

/// The largest untracked file whose lines are counted; a larger one is a file of no lines.
const COUNTED: u64 = 1024 * 1024;

/// What the checkout `cwd` is in changed since it left `base` (their merge base; none is
/// an error, never a guess): the tracked files as `git diff --numstat` counts them, every
/// rename a removal and an addition, a binary file one of no lines; then each untracked file,
/// its lines counted as git would (see [`lines_of`]). The index is never touched.
pub fn stat(cwd: &str, base: &str) -> Result<Stat, String> {
    let git = open(cwd, GIT_WITHIN)?;
    let from = git
        .line(&["merge-base", "--end-of-options", "HEAD", base])
        .ok_or_else(|| format!("HEAD and {base} have no merge base"))?;
    let args = [
        &["diff", "--numstat", "-z", "--no-renames"][..],
        &DIFF,
        &[&from, "--"],
    ]
    .concat();
    let (out, cut) = git.guarded(&args)?.ok_or("git diff did not answer")?;
    let mut stat = numstat(&out, cut);
    let (untracked, more, answered) = untracked_of(&git)?;
    stat.cut |= more || !answered;
    for file in untracked {
        // The files are read by the same deadline as git: past it, the rest is not counted.
        if Instant::now() >= git.by {
            stat.cut = true;
            break;
        }
        if let Some(lines) = lines_of(&git.root.join(&file.path)) {
            stat.files += 1;
            stat.added += lines;
        }
    }
    Ok(stat)
}

/// The records of a `git diff --numstat -z --no-renames`: `<added>\t<removed>\t<path>`, each
/// ending in NUL, `-` for both counts of a binary file. A list `cut` at the cap drops its last
/// record, which may be part of one.
fn numstat(out: &[u8], cut: bool) -> Stat {
    let text = String::from_utf8_lossy(out);
    let mut records: Vec<&str> = text.split('\0').collect();
    // The last is empty, or was cut.
    records.pop();
    let mut stat = Stat {
        cut,
        ..Stat::default()
    };
    for record in records {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(_)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        stat.files += 1;
        stat.added += added.parse::<u64>().unwrap_or(0);
        stat.removed += removed.parse::<u64>().unwrap_or(0);
    }
    stat
}

/// The lines of an untracked file as git would count them, `None` for anything but a file
/// (a symlink, a folder, a pipe). Opened without following a link or waiting on a pipe, and
/// checked on the handle, since what was listed may have been swapped since. Over [`COUNTED`]
/// bytes, or with a NUL in its first 8000 (git's test for binary), it has no lines.
fn lines_of(path: &Path) -> Option<u64> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(COUNTED + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > COUNTED || bytes.iter().take(8000).any(|&b| b == 0) {
        return Some(0);
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    Some(newlines + u64::from(bytes.last().is_some_and(|&b| b != b'\n')))
}

/// The fields of an answer, and the GitHub repository to ask about its pull request.
struct Looked {
    root: String,
    branch: Option<String>,
    base: String,
    files: Vec<ChangedFile>,
    diff: String,
    cut: bool,
    repo: Option<String>,
}

fn look(cwd: &str) -> Result<Looked, String> {
    let git = open(cwd, GIT_WITHIN)?;
    let branch = git.line(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let base = base_of(&git);
    let from = from_of(&git, &base);
    let (mut files, diff, cut) = diff_of(&git, &from)?;
    let repo = git
        .line(&["config", "--get", "remote.origin.url"])
        .and_then(|url| github_repo(&url));
    let (untracked, more, _) = untracked_of(&git)?;
    files.extend(untracked);
    Ok(Looked {
        root: git.root.display().to_string(),
        branch,
        base,
        files,
        diff,
        cut: cut || more,
        repo,
    })
}

/// The checkout `cwd` is in: its `HEAD` commit, and whether nothing is changed or untracked,
/// all of it `within`. Each is `None` when git could not say, never guessed. Untracked files
/// count whatever the checkout's config says. A submodule moved to another commit is a change;
/// the inside of a submodule is not looked into (git would run in a checkout whose config was
/// not read), so a checkout with submodules and nothing else changed is not called clean.
pub(crate) fn head_and_clean(cwd: &str, within: Duration) -> (Option<String>, Option<bool>) {
    let Ok(git) = open(cwd, within) else {
        return (None, None);
    };
    let head = git.line(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
    let args = [
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=dirty",
    ];
    let clean = match git.guarded(&args) {
        Ok(Some((out, false))) if !out.is_empty() => Some(false),
        Ok(Some((_, false))) => (!git.root.join(".gitmodules").exists()).then_some(true),
        _ => None,
    };
    (head, clean)
}

/// `gh` on the daemon's PATH, else where Homebrew puts it: a daemon started by a service may
/// have a PATH without it.
fn gh() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .map(|dir| dir.join("gh"))
        .find(|gh| gh.is_file())
}

/// A pull request as `gh --json number,state,statusCheckRollup` describes one.
fn pull_request(pr: &serde_json::Value) -> Option<PullRequest> {
    let rollup = pr["statusCheckRollup"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    // A check run says how it ended, a status context says its state.
    let word = |c: &serde_json::Value| {
        c["conclusion"]
            .as_str()
            .filter(|w| !w.is_empty())
            .or(c["state"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let failed = [
        "FAILURE",
        "ERROR",
        "CANCELLED",
        "TIMED_OUT",
        "ACTION_REQUIRED",
    ];
    let passed = ["SUCCESS", "NEUTRAL", "SKIPPED"];
    let checks = if rollup.is_empty() {
        "none"
    } else if rollup.iter().any(|c| failed.contains(&word(c).as_str())) {
        "failing"
    } else if rollup.iter().all(|c| passed.contains(&word(c).as_str())) {
        "passing"
    } else {
        "pending"
    };
    Some(PullRequest {
        number: pr["number"].as_u64()?,
        state: pr["state"].as_str()?.to_ascii_lowercase(),
        checks: checks.into(),
    })
}

/// The branch's newest pull request as `gh` at `gh` tells it: `Some(None)` when gh answered
/// that there is none, `None` when it did not answer. Asked by repository, so gh runs no git
/// in the checkout; `pr list` says "none" as `[]` with exit 0, where `pr view` fails.
fn ask_gh(gh: &Path, repo: &str, branch: &str) -> Option<Option<PullRequest>> {
    let mut cmd = Command::new(gh);
    cmd.args([
        "pr", "list", "--repo", repo, "--state", "all", "--limit", "1",
    ])
    .args(["--json", "number,state,statusCheckRollup"])
    .arg(format!("--head={branch}"))
    .current_dir(Path::new("/"))
    .env("GH_PROMPT_DISABLED", "1");
    match run_by(&mut cmd, Instant::now() + GH_WITHIN, Some(CAP))? {
        (out, false) => {
            let list: Vec<serde_json::Value> = serde_json::from_slice(&out).ok()?;
            match list.first() {
                Some(pr) => pull_request(pr).map(Some),
                None => Some(None),
            }
        }
        (_, true) => None,
    }
}

/// `Changes` for a terminal whose shell is in `cwd`.
pub(crate) async fn read(id: String, terminal: u32, cwd: Option<String>) -> ControlResponse {
    let Some(cwd) = cwd else {
        return failed(id, format!("there is no terminal {terminal}"));
    };
    let key = cwd.clone();
    let slack = Duration::from_secs(1);
    let looked =
        match crate::blocking::probe_asked(&key, GIT_WITHIN + slack, move || look(&cwd)).await {
            Ok(Ok(looked)) => looked,
            Ok(Err(reason)) => return failed(id, reason),
            Err(_) => return failed(id, "the checkout is not readable right now"),
        };
    let asked = match (looked.repo.clone(), looked.branch.clone(), gh()) {
        (Some(repo), Some(branch), Some(gh)) => {
            crate::blocking::probe_asked(&looked.root, GH_WITHIN + slack, move || {
                ask_gh(&gh, &repo, &branch)
            })
            .await
            .ok()
            .flatten()
        }
        _ => None,
    };
    let pr_known = asked.is_some();
    let pr = asked.flatten();
    ControlResponse::Changes {
        id,
        root: looked.root,
        branch: looked.branch,
        base: looked.base,
        files: looked.files,
        diff: looked.diff,
        cut: looked.cut,
        pr,
        pr_known,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn output_past_the_cap_is_cut_and_its_command_killed() {
        // Over a pipe's 64 KiB and under the cap: all of it, with no stall on a full pipe.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 100000 /dev/zero"]);
        let (out, cut) = run_by(&mut cmd, soon(), Some(CAP)).expect("ran");
        assert_eq!((out.len(), cut), (100_000, false));
        // `yes` never ends by itself: back well before the deadline means it was killed.
        let began = Instant::now();
        let (out, cut) = run_by(&mut Command::new("yes"), soon(), Some(CAP)).expect("ran");
        assert_eq!((out.len(), cut), (CAP, true));
        assert!(began.elapsed() < Duration::from_secs(5));
        // A command that outlives its deadline is killed with nothing kept.
        let began = Instant::now();
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let by = Instant::now() + Duration::from_millis(300);
        assert_eq!(run_by(&mut cmd, by, Some(CAP)), None);
        assert!(began.elapsed() < Duration::from_secs(5));
        // And what a command left behind in its group goes when it is done.
        let dir = tempfile::tempdir().expect("dir");
        let pid = dir.path().join("pid");
        let mut cmd = Command::new("sh");
        let script = format!("sleep 30 & echo $! > '{}'", pid.display());
        cmd.args(["-c", &script]);
        assert_eq!(run_by(&mut cmd, soon(), None), Some((Vec::new(), false)));
        let pid: i32 = std::fs::read_to_string(&pid)
            .expect("pid")
            .trim()
            .parse()
            .expect("a pid");
        let gone_by = Instant::now() + Duration::from_secs(2);
        // SAFETY: signal 0 only asks whether the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "{pid} left running");
        // One that left the group and holds the pipe: what came before is kept.
        let mut cmd = Command::new("sh");
        let away = r#"echo hi; perl -MPOSIX -e 'POSIX::setsid(); sleep 5' &"#;
        cmd.args(["-c", away]);
        assert_eq!(
            run_by(&mut cmd, soon(), None),
            Some((b"hi\n".to_vec(), false))
        );
    }

    #[test]
    fn every_filter_named_is_found() {
        let config = b"core.bare\nfalse\0filter.lfs.clean\ngit-lfs clean\0filter.a=b.process\nx\0filter.lfs.required\ntrue\0diff.y.textconv\nz\0";
        assert_eq!(filters_in(config), ["a=b", "lfs"]);
    }

    #[test]
    fn checks_are_told_in_one_word() {
        let pr = |rollup: &str| {
            let json = format!(r#"{{"number":7,"state":"OPEN","statusCheckRollup":{rollup}}}"#);
            let json = serde_json::from_str(&json).expect("json");
            pull_request(&json).map(|p| (p.number, p.state, p.checks))
        };
        let open = |checks: &str| Some((7, "open".to_string(), checks.to_string()));
        assert_eq!(pr("[]"), open("none"));
        assert_eq!(
            pr(r#"[{"conclusion":"SUCCESS"},{"state":"SUCCESS"}]"#),
            open("passing")
        );
        assert_eq!(
            pr(r#"[{"conclusion":"SUCCESS"},{"conclusion":"","status":"IN_PROGRESS"}]"#),
            open("pending")
        );
        assert_eq!(
            pr(r#"[{"state":"PENDING"},{"conclusion":"FAILURE"}]"#),
            open("failing")
        );
        assert_eq!(pull_request(&serde_json::json!({"message": "x"})), None);
    }

    #[test]
    fn gh_answering_none_is_told_from_gh_not_answering() {
        let dir = tempfile::tempdir().expect("dir");
        let fake = |name: &str, script: &str| {
            let gh = dir.path().join(name);
            std::fs::write(&gh, format!("#!/bin/sh\n{script}\n")).expect("gh");
            let exec = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(&gh, exec).expect("chmod");
            gh
        };
        let none = fake("none", "echo '[]'");
        assert_eq!(ask_gh(&none, "o/n", "feat/x"), Some(None));
        let one = fake(
            "one",
            r#"echo '[{"number":3,"state":"MERGED","statusCheckRollup":[]}]'"#,
        );
        let merged = ask_gh(&one, "o/n", "feat/x")
            .flatten()
            .map(|p| (p.number, p.state));
        assert_eq!(merged, Some((3, "merged".to_string())));
        let failing = fake("failing", "echo 'not logged in' >&2; exit 4");
        assert_eq!(ask_gh(&failing, "o/n", "feat/x"), None);
    }

    /// git for a test's own setup, with no config but the repository's.
    fn git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository on `trunk` with one commit of `a.txt`.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("r");
        std::fs::create_dir(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "trunk"]);
        std::fs::write(repo.join("a.txt"), "one\n").expect("a");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        (dir, repo)
    }

    fn looked(repo: &Path) -> Looked {
        look(&repo.display().to_string()).expect("looked")
    }

    #[test]
    fn the_base_is_origin_head_else_main_else_head() {
        let (_dir, repo) = repo();
        // No main and no origin: against HEAD, the uncommitted only.
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("a");
        let l = looked(&repo);
        assert_eq!(
            (l.branch.as_deref(), l.base.as_str()),
            (Some("trunk"), "HEAD")
        );
        assert!(l.diff.contains("+two\n"), "{}", l.diff);
        // An origin/HEAD naming no commit is no base.
        let gone = "refs/remotes/origin/gone";
        git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", gone]);
        assert_eq!(looked(&repo).base, "HEAD");
        // origin/HEAD names the base, and a detached HEAD has no branch.
        git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        let target = "refs/remotes/origin/trunk";
        git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", target]);
        git(&repo, &["checkout", "-q", "--detach"]);
        git(&repo, &["commit", "-q", "-am", "second"]);
        let l = looked(&repo);
        assert_eq!((l.branch, l.base.as_str()), (None, "origin/trunk"));
        assert!(l.diff.contains("+two\n"), "the commit since: {}", l.diff);
        assert_eq!(
            l.files,
            [ChangedFile {
                path: "a.txt".into(),
                status: "M".into()
            }]
        );
    }

    #[test]
    fn a_base_never_met_falls_back_to_head() {
        let (_dir, repo) = repo();
        git(&repo, &["checkout", "-q", "--orphan", "main"]);
        git(&repo, &["commit", "-q", "-m", "elsewhere"]);
        git(&repo, &["checkout", "-q", "trunk"]);
        std::fs::write(repo.join("a.txt"), "one\nthree\n").expect("a");
        let l = looked(&repo);
        assert_eq!(l.base, "main");
        // Not the empty tree, which would show `one` as added too.
        assert!(
            l.diff.contains("+three\n") && !l.diff.contains("+one\n"),
            "{}",
            l.diff
        );
    }

    #[test]
    fn a_config_cut_short_is_refused() {
        let (_dir, repo) = repo();
        let mut config = std::fs::read_to_string(repo.join(".git/config")).expect("config");
        for i in 0..4000 {
            config.push_str(&format!("[pad \"p{i}\"]\n\tv = {}\n", "x".repeat(64)));
        }
        std::fs::write(repo.join(".git/config"), config).expect("write");
        assert_eq!(
            look(&repo.display().to_string()).err().as_deref(),
            Some(UNREAD)
        );
    }

    #[test]
    fn a_stat_counts_lines_since_the_base_as_git_would() {
        let (_dir, repo) = repo();
        std::fs::write(repo.join("c.txt"), "x\ny\nz\n").expect("c");
        std::fs::write(repo.join("bin2.dat"), b"\0x").expect("bin2");
        git(&repo, &["add", "c.txt", "bin2.dat"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["update-ref", "refs/qoop/race/x", "HEAD"]);
        // A rename is a removal and an addition; a binary file is one of no lines.
        git(&repo, &["mv", "c.txt", "d.txt"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("a");
        std::fs::write(repo.join("bin2.dat"), b"\0y").expect("bin2");
        // Untracked: lines as git counts them, a binary or an oversized file of none, a
        // symlink not at all.
        std::fs::write(repo.join("new.txt"), "1\n2").expect("new");
        std::fs::write(repo.join("bin.dat"), b"a\0b\n").expect("bin");
        std::fs::write(repo.join("big.txt"), "l\n".repeat(600 * 1024)).expect("big");
        std::os::unix::fs::symlink("a.txt", repo.join("link")).expect("link");
        let before = git(&repo, &["diff", "--cached", "--name-only"]);
        let got = stat(&repo.display().to_string(), "refs/qoop/race/x").expect("stat");
        assert_eq!(
            got,
            Stat {
                files: 7,
                added: 6,
                removed: 3,
                cut: false
            }
        );
        assert_eq!(
            git(&repo, &["diff", "--cached", "--name-only"]),
            before,
            "the index is never touched"
        );
    }

    #[test]
    fn a_stat_against_a_base_never_met_is_an_error() {
        let (_dir, repo) = repo();
        git(&repo, &["checkout", "-q", "--orphan", "elsewhere"]);
        git(&repo, &["commit", "-q", "-m", "elsewhere"]);
        git(&repo, &["update-ref", "refs/qoop/race/x", "HEAD"]);
        git(&repo, &["checkout", "-q", "trunk"]);
        let cwd = repo.display().to_string();
        assert!(stat(&cwd, "refs/qoop/race/x").is_err());
        assert!(stat(&cwd, "refs/qoop/race/none").is_err());
    }

    #[test]
    fn a_numstat_cut_short_drops_its_last_record_and_says_so() {
        let whole = numstat(b"1\t2\ta\0-\t-\tb\0", false);
        assert_eq!(
            whole,
            Stat {
                files: 2,
                added: 1,
                removed: 2,
                cut: false
            }
        );
        let cut = numstat(b"1\t2\ta\0-\t-\tb\x003\t4\tc", true);
        assert_eq!(cut, Stat { cut: true, ..whole });
    }
}

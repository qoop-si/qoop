//! `qoop worktree`: a git worktree beside the repository, with a terminal in it.
//!
//! Git is only ever run with an argv, never through a shell, and every path is compared
//! by component after resolving symlinks, so `/tmp` and `/private/tmp` are one place and
//! `/repo-wt-a` is not inside `/repo`.

use std::path::PathBuf;
use std::process::Command;

use qoop_runtime::paths;

use crate::supervise::{self, NewOpts};

/// One entry of `git worktree list --porcelain -z`.
#[derive(Debug, PartialEq)]
pub(crate) struct Worktree {
    path: PathBuf,
    /// The short branch name; `None` when detached or bare.
    pub(crate) branch: Option<String>,
    head: Option<String>,
    bare: bool,
}

/// Parse `git worktree list --porcelain -z`: NUL-terminated fields, an empty field
/// between entries. NUL rather than newlines because a path can hold a newline.
pub(crate) fn parse_worktrees(raw: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in raw.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            out.extend(current.take());
            current = Some(Worktree {
                path: PathBuf::from(path),
                branch: None,
                head: None,
                bare: false,
            });
        } else if let Some(entry) = current.as_mut() {
            if let Some(head) = field.strip_prefix("HEAD ") {
                entry.head = Some(head.to_string());
            } else if let Some(branch) = field.strip_prefix("branch ") {
                let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
                entry.branch = Some(short.to_string());
            } else if field == "bare" {
                entry.bare = true;
            }
        }
    }
    out.extend(current);
    out
}

/// Run git with argv only (never a shell), and return its stdout or its own complaint.
pub(crate) fn git(dir: &std::path::Path, args: &[&std::ffi::OsStr]) -> std::io::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git {}: {}",
            args.first()
                .map_or(String::new(), |a| a.to_string_lossy().into_owned()),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every worktree of the repository the current directory is in, the main one first.
///
/// Git lists the main worktree first from inside any linked one, which is what makes
/// "beside the repository" mean the same thing wherever this runs.
fn worktrees() -> std::io::Result<Vec<Worktree>> {
    let here = std::env::current_dir()?;
    let raw = git(
        &here,
        &["worktree", "list", "--porcelain", "-z"].map(std::ffi::OsStr::new),
    )?;
    let list = parse_worktrees(&raw);
    if list.is_empty() {
        return Err(std::io::Error::other("git listed no worktrees"));
    }
    Ok(list)
}

/// The repository `dir` is in, for reading what was typed there: its main worktree, its
/// git common dir and every live worktree. Refused outside a git repository.
pub(crate) fn repo_of(dir: &std::path::Path) -> std::io::Result<qoop_history::Repo> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"].map(std::ffi::OsStr::new),
    )
    .map_err(|_| std::io::Error::other(format!("{} is not in a git repository", dir.display())))?;
    let raw = git(
        dir,
        &["worktree", "list", "--porcelain", "-z"].map(std::ffi::OsStr::new),
    )?;
    let mut worktrees: Vec<PathBuf> = parse_worktrees(&raw)
        .into_iter()
        .filter(|w| !w.bare)
        .map(|w| w.path)
        .collect();
    if worktrees.is_empty() {
        return Err(std::io::Error::other("git listed no worktrees"));
    }
    let root = worktrees.remove(0);
    Ok(qoop_history::Repo {
        root,
        common: PathBuf::from(common.trim()),
        worktrees,
    })
}

/// Whether the repository at `root` says merging is pre-authorized, in its committed files
/// (AGENTS.md, CLAUDE.md, `.qoop/workflow.md` at `HEAD`), on one line that names merging and
/// "pre-authorized" and holds back nothing ("not", "never", "ask", "need", "word", "unless",
/// or "release": a release PR's merge is a release).
/// Only there may a worker or qass merge: a merge may deploy, and that is the owner's word.
pub(crate) fn merge_authorized(root: &std::path::Path) -> bool {
    ["AGENTS.md", "CLAUDE.md", ".qoop/workflow.md"]
        .iter()
        .any(|f| {
            let spec = format!("HEAD:{f}");
            let shown = git(
                root,
                &[std::ffi::OsStr::new("show"), std::ffi::OsStr::new(&spec)],
            );
            shown.is_ok_and(|t| t.lines().any(granted))
        })
}

/// One line that pre-authorizes merging, with no word on it that takes it back.
fn granted(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("merge")
        && (l.contains("pre-authorized") || l.contains("pre-authorised"))
        && !["not", "never", "ask", "need", "word", "unless", "release"]
            .iter()
            .any(|w| {
                l.split(|c: char| !c.is_alphanumeric())
                    .any(|x| x.starts_with(w))
            })
}

/// `owner/name` when the top-level repository `dir` is in has an `origin` on `github.com`
/// whose owner is the person's, any case, and every push address of that `origin` is the
/// person's too: the person's own repository. The person's owners are this machine's GitHub
/// login and every `qoop.owner` in the global git config (an org or a second account), read
/// with its `includeIf`s so a folder can name its own: scope those with an absolute
/// `gitdir:`, since a relative one or `onbranch:` follows what a repository says about
/// itself. A repository's local config is never read, so no repository can name itself, and
/// an inherited `GIT_CONFIG_GLOBAL` is dropped, so no launch points the lookup elsewhere. A
/// submodule never counts (its superproject decides); a worktree counts as its repository.
/// Anything unread, empty or slower than [`OWN_REPO_TIME`] (no repository, no origin,
/// another host, no `gh`, no login) is `None`. `gh config get` reads a local file: no network.
pub(crate) fn own_repo(dir: &std::path::Path) -> Option<String> {
    own_repo_with(dir, std::ffi::OsStr::new("gh"), None, OWN_REPO_TIME)
}

/// The git and gh calls of [`own_repo`] together, at most: a hook waits on it every request.
const OWN_REPO_TIME: std::time::Duration = std::time::Duration::from_millis(300);

/// `global` stands in for the global git config file and `budget` for [`OWN_REPO_TIME`]
/// (tests); `None` is the real file.
fn own_repo_with(
    dir: &std::path::Path,
    gh: &std::ffi::OsStr,
    global: Option<&std::path::Path>,
    budget: std::time::Duration,
) -> Option<String> {
    let by = std::time::Instant::now() + budget;
    let git = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(dir).args(args);
        match global {
            Some(file) => cmd.env("GIT_CONFIG_GLOBAL", file),
            None => cmd.env_remove("GIT_CONFIG_GLOBAL"),
        };
        run_by(&mut cmd, by)
    };
    let superproject = git(&["rev-parse", "--show-superproject-working-tree"])?;
    if !superproject.trim().is_empty() {
        return None;
    }
    // `get-url` applies `insteadOf`, so the addresses are the ones git really uses.
    let url = git(&["remote", "get-url", "origin"])?;
    let pushes = git(&["remote", "get-url", "--push", "--all", "origin"])?;
    let mut cmd = Command::new(gh);
    cmd.args(["config", "get", "-h", "github.com", "user"]);
    let login = run_by(&mut cmd, by)?;
    let login = login.trim();
    if login.is_empty() {
        return None;
    }
    let addresses: Vec<&str> = std::iter::once(url.as_str())
        .chain(pushes.lines())
        .collect();
    // The named owners are read only when the login does not own every address, so the
    // login's own repositories cost nothing more. Unset (exit 1), unread or past the
    // deadline names no one: the repository is then not own, the safe side.
    let login_owns_all = addresses.iter().all(|a| owned(a.trim(), login).is_some());
    let named = match login_owns_all {
        true => String::new(),
        false => git(&[
            "config",
            "--global",
            "--includes",
            "--get-all",
            "qoop.owner",
        ])
        .unwrap_or_default(),
    };
    let owners: Vec<&str> = std::iter::once(login)
        .chain(named.lines().map(str::trim))
        .filter(|o| !o.is_empty())
        .collect();
    let owned_by_one = |url: &str| owners.iter().find_map(|o| owned(url.trim(), o));
    let repo = owned_by_one(&url)?;
    addresses
        .iter()
        .all(|a| owned_by_one(a).is_some())
        .then_some(repo)
}

/// What `cmd` printed, when it exits 0 by `by`; past it, it and everything it started (its
/// own process group) are killed and reaped, so nothing is left behind.
fn run_by(cmd: &mut Command, by: std::time::Instant) -> Option<String> {
    qoop_runtime::changes::run_by(cmd, by, None)
        .map(|(out, _)| String::from_utf8_lossy(&out).into_owned())
}

/// `owner/name` of a GitHub `url` when its owner is `login` (an empty one matches nothing).
fn owned(url: &str, login: &str) -> Option<String> {
    qoop_runtime::shows::github_repo(url).filter(|repo| {
        repo.split('/')
            .next()
            .is_some_and(|o| o.eq_ignore_ascii_case(login))
    })
}

/// A branch name as one path component: lowercase letters and digits, every other run
/// of characters one dash.
pub(crate) fn worktree_slug(branch: &str) -> String {
    let mut slug = String::new();
    for c in branch.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() { "wt" } else { slug }.to_string()
}

/// Where a new worktree goes when `--path` is not given: `<parent>/<repo>-wt-<slug>`,
/// beside the main worktree. A bare repository's `.git` is not part of its name, and
/// neither is a `.git` directory given as the main path.
fn default_worktree_path(main: &std::path::Path, branch: &str) -> PathBuf {
    let main = if main.file_name().is_some_and(|n| n == ".git") {
        main.parent().unwrap_or(main)
    } else {
        main
    };
    let name = main.file_name().map_or_else(
        || "repo".to_string(),
        |n| n.to_string_lossy().trim_end_matches(".git").to_string(),
    );
    let parent = main.parent().unwrap_or(main);
    parent.join(format!("{name}-wt-{}", worktree_slug(branch)))
}

/// A path with its symlinks resolved, so `/tmp` and `/private/tmp` are one place. A path
/// that cannot be resolved (gone, or not yet made) is compared as written.
fn real(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Which worktree a directory is in: the deepest one containing it, compared by path
/// component after resolving both, so `/repo-wt-a` is not inside `/repo`.
fn worktree_of(roots: &[PathBuf], cwd: &std::path::Path) -> Option<usize> {
    let cwd = real(cwd);
    roots
        .iter()
        .enumerate()
        .filter(|(_, root)| cwd.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(i, _)| i)
}

/// This machine's rows, or none when qoop is not running: with no daemon there are no
/// terminals, which is the true answer and not a failure.
fn session_rows() -> std::io::Result<Vec<qoop_runtime::control::AgentRow>> {
    use qoop_runtime::control::{ControlRequest, ControlResponse};

    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        return Ok(Vec::new());
    }
    let rt = tokio::runtime::Runtime::new()?;
    let answer = rt.block_on(crate::ask_daemon(
        &socket,
        "qoop-worktree",
        &ControlRequest::ListAgents {
            id: crate::request_id("worktree"),
        },
    ))?;
    match answer {
        ControlResponse::Agents { agents, .. } => Ok(agents),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        _ => Err(std::io::Error::other(
            "unexpected answer to the agent listing",
        )),
    }
}

/// `qoop worktree`: a git worktree beside the repository, with a terminal in it.
pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("new") => worktree_new(args),
        Some("ls") => worktree_ls(args),
        Some("rm") => worktree_rm(args),
        Some("help" | "--help" | "-h") => {
            println!("{WORKTREE_USAGE}");
            Ok(())
        }
        _ => Err(crate::usage(WORKTREE_USAGE)),
    }
}

const WORKTREE_USAGE: &str = "usage:\n\
     \x20 qoop worktree new <branch> [--base REF] [--path DIR] [--space ID|NAME] [--name NAME] [-- CMD...]\n\
     \x20 qoop worktree ls [--json]\n\
     \x20 qoop worktree rm <branch|path> [--force]";

/// Parsed `qoop worktree new` arguments. Its own two flags come out first and the rest
/// is `qoop new`'s own parse, so the two cannot drift apart about `--space` or `--`.
struct WorktreeNewOpts {
    branch: String,
    base: Option<String>,
    path: Option<PathBuf>,
    new: NewOpts,
}

fn parse_worktree_new_args(args: &[String]) -> Result<WorktreeNewOpts, String> {
    // A leading dash is refused because git would read it as an option: argv keeps a
    // shell out, not an option.
    let branch = args
        .get(3)
        .filter(|b| !b.starts_with('-'))
        .cloned()
        .ok_or_else(|| WORKTREE_USAGE.to_string())?;
    let mut base = None;
    let mut path = None;
    let mut rest = vec!["qoop".to_string(), "new".to_string()];
    let mut it = args.iter().skip(4);
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("missing value for {arg}; {WORKTREE_USAGE}"))
        };
        match arg.as_str() {
            "--base" => {
                let from = value()?;
                if from.starts_with('-') {
                    return Err(format!("--base cannot start with a dash; {WORKTREE_USAGE}"));
                }
                base = Some(from);
            }
            "--path" => path = Some(PathBuf::from(value()?)),
            "--" => {
                rest.push(arg.clone());
                rest.extend(it.cloned());
                break;
            }
            _ => rest.push(arg.clone()),
        }
    }
    // `new`'s own reason, with this command's usage in place of `new`'s.
    let new = supervise::parse_new_args(&rest).map_err(|e| {
        let why = e.split(';').next().unwrap_or_default();
        format!("{why}; {WORKTREE_USAGE}")
    })?;
    // The directory is the worktree's and the output is the worktree's.
    if new.cwd.is_some() {
        return Err(format!(
            "--cwd does not apply to worktree new; {WORKTREE_USAGE}"
        ));
    }
    Ok(WorktreeNewOpts {
        branch,
        base,
        path,
        new,
    })
}

/// `qoop worktree new`: add the worktree, then open a terminal in it named after the
/// branch. A branch already checked out elsewhere is refused with where, because git's
/// own refusal is about a path the person did not type. A branch that exists only on a
/// remote is handed to git by name, so git makes the local branch tracking it rather than
/// a fresh one from HEAD.
fn worktree_new(args: &[String]) -> std::io::Result<()> {
    let WorktreeNewOpts {
        branch,
        base,
        path,
        mut new,
    } = parse_worktree_new_args(args).map_err(crate::usage)?;
    // Checked before git runs, so qoop not running does not leave a worktree behind.
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    if std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_err() {
        return Err(std::io::Error::other(
            "nothing is awake: `qoop` or `qoop wake` starts it",
        ));
    }
    let list = worktrees()?;
    let main = list[0].path.clone();
    if let Some(taken) = list.iter().find(|w| w.branch.as_deref() == Some(&branch)) {
        return Err(std::io::Error::other(format!(
            "branch {branch} is already checked out at {}",
            taken.path.display()
        )));
    }
    let local = git(
        &main,
        &[
            "show-ref".as_ref(),
            "--verify".as_ref(),
            "--quiet".as_ref(),
            format!("refs/heads/{branch}").as_ref(),
        ],
    )
    .is_ok();
    let remote = !local
        && git(
            &main,
            &[
                "for-each-ref",
                "--format=%(refname:strip=3)",
                "refs/remotes",
            ]
            .map(std::ffi::OsStr::new),
        )?
        .lines()
        .any(|name| name == branch);
    if (local || remote) && base.is_some() {
        return Err(std::io::Error::other(format!(
            "branch {branch} exists; --base only applies to a new branch"
        )));
    }
    let here = std::env::current_dir()?;
    let path = path.map_or_else(|| default_worktree_path(&main, &branch), |p| here.join(p));
    let mut add: Vec<&std::ffi::OsStr> = vec!["worktree".as_ref(), "add".as_ref()];
    if local || remote {
        add.extend([path.as_os_str(), branch.as_ref()]);
    } else {
        add.extend(["-b".as_ref(), branch.as_ref(), path.as_os_str()]);
        add.extend(base.as_deref().map(std::ffi::OsStr::new));
    }
    git(&main, &add)?;
    // Resolved once it exists, so `--path ../x` is not recorded as a tab's directory.
    let path = real(&path);
    println!("worktree={} branch={branch}", path.display());
    new.cwd = Some(path);
    new.name = new.name.or_else(|| Some(branch.clone()));
    supervise::open_terminal(&new, |terminal| println!("terminal={terminal}"))
}

/// `qoop worktree ls`: every worktree, and this machine's terminals inside each.
fn worktree_ls(args: &[String]) -> std::io::Result<()> {
    let json = match args.get(3).map(String::as_str) {
        None => false,
        Some("--json") if args.len() == 4 => true,
        _ => return Err(crate::usage(WORKTREE_USAGE)),
    };
    let list = worktrees()?;
    let rows = session_rows()?;
    let roots: Vec<PathBuf> = list.iter().map(|w| real(&w.path)).collect();
    let mut inside: Vec<Vec<&qoop_runtime::control::AgentRow>> = vec![Vec::new(); list.len()];
    for row in &rows {
        if let Some(i) = row
            .cwd
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .and_then(|c| worktree_of(&roots, std::path::Path::new(c)))
        {
            inside[i].push(row);
        }
    }
    if json {
        let out: Vec<_> = list
            .iter()
            .zip(&inside)
            .map(|(w, rows)| {
                serde_json::json!({
                    "path": w.path,
                    "branch": w.branch,
                    "head": w.head,
                    "bare": w.bare,
                    "terminals": rows,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&out).map_err(std::io::Error::other)?
        );
        return Ok(());
    }
    for (w, rows) in list.iter().zip(&inside) {
        println!(
            "path={} branch={} terminals={}",
            w.path.display(),
            w.branch.as_deref().unwrap_or("-"),
            rows.len()
        );
        for row in rows {
            println!("  {}", crate::agent_line(row));
        }
    }
    Ok(())
}

/// A process standing inside a worktree.
struct Occupant {
    pid: i32,
    program: String,
    /// Whether it is a qoop daemon.
    daemon: bool,
}

/// Every process of this user whose directory is inside `root`.
///
/// Read from the processes themselves, never from the index: the index learns a
/// terminal's directory on a throttle, misses an agent a subshell started somewhere else,
/// and knows nothing of a shell outside qoop. A process
/// of another user is not readable and is not ours to protect. qoop's own clients are left
/// out, this one included: an interface or a `wait` started from inside holds no work
/// there. A qoop daemon is kept, because its directory is where every terminal it opens
/// without one starts. Fails closed when the scan cannot even see this process, because
/// an empty answer from a scan that does not work reads exactly like "nobody is inside".
fn processes_inside(root: &std::path::Path) -> std::io::Result<Vec<Occupant>> {
    use qoop_pty::sys;

    let me = std::process::id() as i32;
    let pids = sys::scan_all_pids();
    if !pids.contains(&me) || sys::cwd_of(qoop_pty::Pid(me)).is_none() {
        return Err(std::io::Error::other(
            "cannot list the processes on this machine, so cannot tell whether the worktree \
             is in use; not removing it",
        ));
    }
    Ok(pids
        .into_iter()
        .filter_map(|pid| {
            let cwd = sys::cwd_of(qoop_pty::Pid(pid))?;
            if !real(&cwd).starts_with(root) {
                return None;
            }
            let program = sys::program_of(qoop_pty::Pid(pid))
                .and_then(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "?".to_string());
            let mut daemon = false;
            if program == "qoop" {
                let argv = sys::argv_of(qoop_pty::Pid(pid));
                if !argv.iter().any(|a| a == "daemon") {
                    return None;
                }
                daemon = true;
            }
            Some(Occupant {
                pid,
                program,
                daemon,
            })
        })
        .collect())
}

/// `qoop worktree rm`: remove a linked worktree, keeping its branch.
///
/// Refused while any process of this user stands inside it, whatever started it; the
/// index only names which of those are qoop terminals. `--force` is for a dirty tree and
/// never for a live process: removing the directory under a running agent loses its work
/// just as surely. A dirty tree is refused without `--force` before git is asked, so the
/// message names the fix.
fn worktree_rm(args: &[String]) -> std::io::Result<()> {
    let target = args
        .get(3)
        .filter(|t| !t.starts_with('-'))
        .ok_or_else(|| crate::usage(WORKTREE_USAGE))?;
    let mut force = false;
    for arg in args.iter().skip(4) {
        match arg.as_str() {
            "--force" => force = true,
            other => {
                return Err(crate::usage(format!(
                    "unknown flag {other}; {WORKTREE_USAGE}"
                )));
            }
        }
    }
    let list = worktrees()?;
    let wanted = real(&std::env::current_dir()?.join(target));
    let Some(at) = list
        .iter()
        .position(|w| w.branch.as_deref() == Some(target.as_str()))
        .or_else(|| list.iter().position(|w| real(&w.path) == wanted))
    else {
        return Err(std::io::Error::other(format!(
            "no worktree for branch or path {target}"
        )));
    };
    if at == 0 {
        return Err(std::io::Error::other(
            "that is the main worktree; only a linked one is removed",
        ));
    }
    let tree = &list[at];
    if !tree.path.exists() {
        return Err(std::io::Error::other(format!(
            "{} is already gone; `git worktree prune` forgets it",
            tree.path.display()
        )));
    }
    let root = real(&tree.path);
    let live = processes_inside(&root)?;
    if let Some(Occupant { pid, .. }) = live.iter().find(|o| o.daemon) {
        return Err(std::io::Error::other(format!(
            "a qoop daemon (pid {pid}) runs from inside {}: stop it or start it elsewhere, \
             because new terminals start in its directory",
            tree.path.display()
        )));
    }
    if live
        .iter()
        .any(|o| o.pid as u32 == std::os::unix::process::parent_id())
    {
        return Err(std::io::Error::other(format!(
            "the shell this ran from is inside {}: run it from outside the worktree",
            tree.path.display()
        )));
    }
    if !live.is_empty() {
        let terminals: Vec<String> = session_rows()?
            .iter()
            .filter(|r| r.state != "sleeping")
            .filter(|r| {
                r.cwd
                    .as_deref()
                    .is_some_and(|c| real(std::path::Path::new(c)).starts_with(&root))
            })
            .map(|r| r.terminal.to_string())
            .collect();
        let named = if terminals.is_empty() {
            String::new()
        } else {
            format!(" (qoop terminal {})", terminals.join(", "))
        };
        return Err(std::io::Error::other(format!(
            "{} is in use by pid {}{named}: close them first; --force does not override a \
             running process",
            tree.path.display(),
            live.iter()
                .map(|o| format!("{} ({})", o.pid, o.program))
                .collect::<Vec<_>>()
                .join(", pid ")
        )));
    }
    let status = git(
        &tree.path,
        &["status", "--porcelain", "--untracked-files=all"].map(std::ffi::OsStr::new),
    );
    match status {
        Ok(changes) if !changes.trim().is_empty() && !force => {
            return Err(std::io::Error::other(format!(
                "{} has uncommitted changes: commit them, or pass --force to lose them",
                tree.path.display()
            )));
        }
        Err(e) if !force => return Err(e),
        _ => {}
    }
    let mut remove: Vec<&std::ffi::OsStr> = vec!["worktree".as_ref(), "remove".as_ref()];
    if force {
        remove.push("--force".as_ref());
    }
    remove.push(tree.path.as_os_str());
    git(&list[0].path, &remove)?;
    println!(
        "removed {}; branch {} kept",
        tree.path.display(),
        tree.branch.as_deref().unwrap_or("-")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn worktree_porcelain_reads_main_first_with_branch_bare_and_detached() {
        let raw = "worktree /src/repo.git\0bare\0\0\
                   worktree /src/repo wt/a b\0HEAD 1111\0branch refs/heads/feat/x\0\0\
                   worktree /src/detached\0HEAD 2222\0detached\0\0";
        let list = parse_worktrees(raw);
        assert_eq!(
            list,
            vec![
                Worktree {
                    path: PathBuf::from("/src/repo.git"),
                    branch: None,
                    head: None,
                    bare: true,
                },
                Worktree {
                    path: PathBuf::from("/src/repo wt/a b"),
                    branch: Some("feat/x".into()),
                    head: Some("1111".into()),
                    bare: false,
                },
                Worktree {
                    path: PathBuf::from("/src/detached"),
                    branch: None,
                    head: Some("2222".into()),
                    bare: false,
                },
            ]
        );
    }

    #[test]
    fn a_new_worktree_sits_beside_the_repo_named_without_git() {
        assert_eq!(worktree_slug("Feat/Login--Page_2"), "feat-login-page-2");
        assert_eq!(worktree_slug("///"), "wt");
        assert_eq!(
            default_worktree_path(Path::new("/src/my repo"), "feat/x"),
            PathBuf::from("/src/my repo-wt-feat-x")
        );
        assert_eq!(
            default_worktree_path(Path::new("/src/repo.git"), "x"),
            PathBuf::from("/src/repo-wt-x")
        );
        assert_eq!(
            default_worktree_path(Path::new("/src/repo/.git"), "x"),
            PathBuf::from("/src/repo-wt-x")
        );
    }

    #[test]
    fn a_directory_belongs_to_the_deepest_worktree_by_path_component() {
        let roots = vec![
            PathBuf::from("/nowhere/repo"),
            PathBuf::from("/nowhere/repo/nested"),
            PathBuf::from("/nowhere/repo-wt-a"),
        ];
        assert_eq!(worktree_of(&roots, Path::new("/nowhere/repo/src")), Some(0));
        assert_eq!(
            worktree_of(&roots, Path::new("/nowhere/repo/nested/x")),
            Some(1)
        );
        assert_eq!(
            worktree_of(&roots, Path::new("/nowhere/repo-wt-a")),
            Some(2)
        );
        assert_eq!(worktree_of(&roots, Path::new("/nowhere/repo-wt-ab")), None);
    }

    #[test]
    fn an_own_repo_is_an_origin_on_github_whose_owner_is_the_login() {
        for url in [
            "git@github.com:pat/qoop-tui.git",
            "git@github.com:pat/qoop-tui",
            "ssh://git@github.com/pat/qoop-tui.git",
            "ssh://git@github.com:22/pat/qoop-tui/",
            "https://github.com/pat/qoop-tui",
            "https://github.com/pat/qoop-tui.git/",
            "https://us:token@github.com/pat/qoop-tui.git",
            "https://GitHub.com/PAT/qoop-tui",
        ] {
            assert!(
                owned(url, "Us").is_some_and(|r| r.eq_ignore_ascii_case("pat/qoop-tui")),
                "{url}"
            );
        }
        assert_eq!(
            owned("git@github.com:pat/qoop.si.git", "us").as_deref(),
            Some("pat/qoop.si")
        );
        for (url, login) in [
            ("git@github.com:bigco/app.git", "us"),
            ("git@github.example.com:pat/x.git", "us"),
            ("https://github.com.evil.io/pat/x", "us"),
            ("https://gitlab.com/pat/x", "us"),
            ("git@github.com:pat/x.git", ""),
            ("git@github.com:pat/x/y.git", "us"),
            ("git@github.com:pat/.git", "us"),
            ("/src/pat/x", "us"),
            ("./a/b:pat/x", "us"),
            ("https://evil.com#@github.com/pat/x", "us"),
            ("https://evil.com?@github.com/pat/x", "us"),
            ("https://evil.com#/@github.com/pat/x", "us"),
            ("https://evil.com\\@github.com/pat/x", "us"),
            ("https://evil.com @github.com/pat/x", "us"),
            ("https://evil.com\t@github.com/pat/x", "us"),
            ("https://github.com/pat/x?y=1", "us"),
            ("https://github.com/pat/x#frag", "us"),
            ("", "us"),
        ] {
            assert_eq!(owned(url, login), None, "{url} as {login:?}");
        }
    }

    /// A deadline a loaded test machine never misses by accident; the deadline itself is
    /// tested with a hanging `gh` below.
    const ROOMY: std::time::Duration = std::time::Duration::from_secs(10);

    /// Real repositories: an own one and its worktree count; a submodule pointing at the
    /// owner inside a stranger's repository does not, nor anything with `gh` missing.
    #[test]
    fn only_a_top_level_repo_or_its_worktree_is_own() {
        let dir = tempfile::tempdir().expect("dir");
        let at = |p: &str| dir.path().join(p);
        let run = |d: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(d)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "protocol.file.allow=always"])
                .args(args)
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        let gh = at("gh");
        std::fs::write(&gh, "#!/bin/sh\necho Us\n").expect("gh");
        std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        for (name, origin) in [
            ("own", "git@github.com:pat/own.git"),
            ("org", "https://github.com/bigco/app"),
        ] {
            std::fs::create_dir_all(at(name)).expect("repo");
            run(&at(name), &["init", "-q"]);
            run(&at(name), &["commit", "-q", "--allow-empty", "-m", "x"]);
            run(&at(name), &["remote", "add", "origin", origin]);
        }
        let wt = at("own-wt");
        run(
            &at("own"),
            &["worktree", "add", "-q", wt.to_str().expect("utf8")],
        );
        run(
            &at("org"),
            &[
                "submodule",
                "add",
                "-q",
                at("own").to_str().expect("utf8"),
                "sub",
            ],
        );
        let sub = at("org/sub");
        run(
            &sub,
            &["remote", "set-url", "origin", "git@github.com:pat/own.git"],
        );
        let no_global = at("gitconfig");
        std::fs::write(&no_global, "").expect("global");
        let own = |d: &Path| own_repo_with(d, gh.as_os_str(), Some(&no_global), ROOMY);
        assert_eq!(own(&at("own")).as_deref(), Some("pat/own"));
        assert_eq!(own(&wt).as_deref(), Some("pat/own"), "a worktree counts");
        assert_eq!(own(&at("org")), None, "an org's repo");
        assert_eq!(own(&sub), None, "a submodule never counts");
        // Every push address must be the login's too.
        run(
            &at("own"),
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                "origin",
                "git@github.com:pat/own.git",
            ],
        );
        run(
            &at("own"),
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                "origin",
                "https://github.com/PAT/mirror",
            ],
        );
        assert_eq!(own(&at("own")).as_deref(), Some("pat/own"), "own pushes");
        run(
            &at("own"),
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                "origin",
                "git@github.com:acme/x.git",
            ],
        );
        assert_eq!(own(&at("own")), None, "one push address elsewhere");
        assert_eq!(own(&wt), None, "the worktree shares it");
        run(
            &at("own"),
            &["remote", "set-url", "--delete", "--push", "origin", "acme"],
        );
        assert_eq!(own(&at("own")).as_deref(), Some("pat/own"));
        assert_eq!(own(dir.path()), None, "no repository");
        let missing = at("no-gh");
        assert_eq!(
            own_repo_with(&at("own"), missing.as_os_str(), Some(&no_global), ROOMY),
            None
        );
        std::fs::write(&gh, "#!/bin/sh\necho\n").expect("gh");
        assert_eq!(own(&at("own")), None, "no login");
        std::fs::write(&gh, "#!/bin/sh\necho us\nexit 1\n").expect("gh");
        assert_eq!(own(&at("own")), None, "gh failed");
    }

    /// An owner the person names in the global config (here through an `includeIf` for one
    /// folder) counts like the login; the same org elsewhere does not, and a repository
    /// cannot name itself in its own config.
    #[test]
    fn a_named_owner_counts_only_from_the_global_config() {
        let dir = tempfile::tempdir().expect("dir");
        // includeIf compares real paths: macOS temp dirs sit behind a /var symlink.
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let at = |p: &str| root.join(p);
        let git = |d: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(d)
                .args(args)
                .status()
                .expect("git")
                .success();
            assert!(ok, "git {args:?}");
        };
        let gh = at("gh");
        std::fs::write(&gh, "#!/bin/sh\necho us\n").expect("gh");
        std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        for name in ["named/qoop", "elsewhere/qoop"] {
            std::fs::create_dir_all(at(name)).expect("repo");
            git(&at(name), &["init", "-q"]);
            git(
                &at(name),
                &["remote", "add", "origin", "git@github.com:qoop-si/qoop.git"],
            );
        }
        std::fs::write(at("named.gitconfig"), "[qoop]\n\towner = qoop-si\n").expect("named");
        let global = at("gitconfig");
        std::fs::write(
            &global,
            format!(
                "[includeIf \"gitdir:{}/\"]\n\tpath = {}\n",
                at("named").display(),
                at("named.gitconfig").display()
            ),
        )
        .expect("global");
        let own = |d: &Path| own_repo_with(d, gh.as_os_str(), Some(&global), ROOMY);
        assert_eq!(own(&at("named/qoop")).as_deref(), Some("qoop-si/qoop"));
        assert_eq!(
            own(&at("elsewhere/qoop")),
            None,
            "the org outside the folder"
        );
        git(&at("elsewhere/qoop"), &["config", "qoop.owner", "qoop-si"]);
        assert_eq!(
            own(&at("elsewhere/qoop")),
            None,
            "a repository naming itself"
        );
        // Every push address must be one of the person's: one elsewhere among them is enough.
        let push = |d: &str, url: &str| {
            git(
                &at(d),
                &["remote", "set-url", "--add", "--push", "origin", url],
            );
        };
        push("named/qoop", "git@github.com:qoop-si/qoop.git");
        assert_eq!(own(&at("named/qoop")).as_deref(), Some("qoop-si/qoop"));
        push("named/qoop", "git@github.com:acme/x.git");
        assert_eq!(own(&at("named/qoop")), None, "one push address elsewhere");
        // The login's origin with a push to a named owner is own: the names are read then too.
        std::fs::create_dir_all(at("named/mixed")).expect("repo");
        git(&at("named/mixed"), &["init", "-q"]);
        git(
            &at("named/mixed"),
            &["remote", "add", "origin", "git@github.com:pat/own.git"],
        );
        push("named/mixed", "git@github.com:qoop-si/mirror.git");
        assert_eq!(own(&at("named/mixed")).as_deref(), Some("pat/own"));
        // No login is never own, even where a named owner alone would do.
        std::fs::create_dir_all(at("named/org")).expect("repo");
        git(&at("named/org"), &["init", "-q"]);
        git(
            &at("named/org"),
            &["remote", "add", "origin", "git@github.com:qoop-si/org.git"],
        );
        assert_eq!(own(&at("named/org")).as_deref(), Some("qoop-si/org"));
        std::fs::write(&gh, "#!/bin/sh\necho\n").expect("gh");
        assert_eq!(own(&at("named/org")), None, "no login");
    }

    /// A `gh` that hangs is not own within the deadline, and it and what it started are gone.
    #[test]
    fn a_slow_gh_is_not_own_and_leaves_nothing_running() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("own");
        std::fs::create_dir_all(&repo).expect("repo");
        for args in [
            &["init", "-q"][..],
            &["remote", "add", "origin", "git@github.com:pat/own.git"],
        ] {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .expect("git")
                .success();
            assert!(ok, "git {args:?}");
        }
        let pids = dir.path().join("pids");
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nsleep 30 &\necho $$ $! > '{}'\nwait\necho us\n",
                pids.display()
            ),
        )
        .expect("gh");
        std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        // The gh stub sleeps 30 s: back long before it means the deadline held. Loose on purpose,
        // a loaded machine runs the git calls slowly too.
        let start = std::time::Instant::now();
        assert_eq!(
            own_repo_with(&repo, gh.as_os_str(), None, OWN_REPO_TIME),
            None
        );
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
        // The kill on its own, with time for the stub to note its pids: nothing it started stays.
        let _ = std::fs::remove_file(&pids);
        let by = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        assert_eq!(run_by(&mut Command::new(&gh), by), None);
        let pids = std::fs::read_to_string(&pids).expect("gh started");
        let alive = |pid: i32| {
            // SAFETY: signal 0 only asks whether the process exists.
            unsafe { libc::kill(pid, 0) == 0 }
        };
        let gone_by = std::time::Instant::now() + std::time::Duration::from_secs(2);
        for pid in pids
            .split_whitespace()
            .map(|p| p.parse::<i32>().expect("pid"))
        {
            while alive(pid) && std::time::Instant::now() < gone_by {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(!alive(pid), "process {pid} left running");
        }
    }

    #[test]
    fn worktree_new_shares_the_new_parse_and_keeps_its_own_flags() {
        let opts = parse_worktree_new_args(&argv(&[
            "qoop", "worktree", "new", "feat/x", "--base", "main", "--space", "work", "--path",
            "../x", "--", "claude", "--resume",
        ]))
        .unwrap();
        assert_eq!(opts.branch, "feat/x");
        assert_eq!(opts.base.as_deref(), Some("main"));
        assert_eq!(opts.path, Some(PathBuf::from("../x")));
        assert_eq!(opts.new.space.as_deref(), Some("work"));
        assert_eq!(opts.new.command, vec!["claude", "--resume"]);
        for bad in [
            &["qoop", "worktree", "new"][..],
            &["qoop", "worktree", "new", "-b"],
            &["qoop", "worktree", "new", "x", "--base", "-evil"],
            &["qoop", "worktree", "new", "x", "--cwd", "/tmp"],
            &["qoop", "worktree", "new", "x", "--json"],
            &["qoop", "worktree", "new", "x", "--bogus"],
        ] {
            assert!(
                parse_worktree_new_args(&argv(bad)).is_err(),
                "{bad:?} parsed"
            );
        }
    }

    /// Merging is pre-authorized only by a repository file that says so, as qoop's do.
    #[test]
    fn only_a_repo_file_pre_authorizes_merging() {
        assert!(granted(
            "2. Commit, push, opening a PR and rebase-and-merge are pre-authorized."
        ));
        assert!(!granted("Merge after review."));
        assert!(!granted(
            "Commit and push are pre-authorized; merging needs his word."
        ));
        assert!(!granted("Merging is never pre-authorized."));
        assert!(!granted("Merging the release PR is pre-authorized."));
        let dir = tempfile::tempdir().expect("dir");
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git")
        };
        run(&["init", "-q"]);
        assert!(!merge_authorized(dir.path()), "no commit, no grant");
        std::fs::write(
            dir.path().join("AGENTS.md"),
            "2. Commit, push, opening a PR and rebase-and-merge are pre-authorized.\n",
        )
        .expect("write");
        assert!(
            !merge_authorized(dir.path()),
            "uncommitted text grants nothing"
        );
        run(&["add", "AGENTS.md"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "commit",
            "-qm",
            "rules",
        ]);
        assert!(merge_authorized(dir.path()));
    }
}

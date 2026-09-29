//! `qoro worktree`: a git worktree beside the repository, with a terminal in it.
//!
//! Git is only ever run with an argv, never through a shell, and every path is compared
//! by component after resolving symlinks, so `/tmp` and `/private/tmp` are one place and
//! `/repo-wt-a` is not inside `/repo`.

use std::path::PathBuf;
use std::process::Command;

use qoro_runtime::paths;

use crate::supervise::{self, NewOpts};

/// One entry of `git worktree list --porcelain -z`.
#[derive(Debug, PartialEq)]
struct Worktree {
    path: PathBuf,
    /// The short branch name; `None` when detached or bare.
    branch: Option<String>,
    head: Option<String>,
    bare: bool,
}

/// Parse `git worktree list --porcelain -z`: NUL-terminated fields, an empty field
/// between entries. NUL rather than newlines because a path can hold a newline.
fn parse_worktrees(raw: &str) -> Vec<Worktree> {
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
fn git(dir: &std::path::Path, args: &[&std::ffi::OsStr]) -> std::io::Result<String> {
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

/// A branch name as one path component: lowercase letters and digits, every other run
/// of characters one dash.
fn worktree_slug(branch: &str) -> String {
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

/// This machine's rows, or none when qoro is not running: with no daemon there are no
/// terminals, which is the true answer and not a failure.
fn session_rows() -> std::io::Result<Vec<qoro_runtime::control::AgentRow>> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        return Ok(Vec::new());
    }
    let rt = tokio::runtime::Runtime::new()?;
    let answer = rt.block_on(crate::ask_daemon(
        &socket,
        "qoro-worktree",
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

/// `qoro worktree`: a git worktree beside the repository, with a terminal in it.
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
     \x20 qoro worktree new <branch> [--base REF] [--path DIR] [--space ID|NAME] [--name NAME] [-- CMD...]\n\
     \x20 qoro worktree ls [--json]\n\
     \x20 qoro worktree rm <branch|path> [--force]";

/// Parsed `qoro worktree new` arguments. Its own two flags come out first and the rest
/// is `qoro new`'s own parse, so the two cannot drift apart about `--space` or `--`.
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
    let mut rest = vec!["qoro".to_string(), "new".to_string()];
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

/// `qoro worktree new`: add the worktree, then open a terminal in it named after the
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
    // Checked before git runs, so qoro not running does not leave a worktree behind.
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    if std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_err() {
        return Err(std::io::Error::other(
            "nothing is awake: `qoro` or `qoro wake` starts it",
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

/// `qoro worktree ls`: every worktree, and this machine's terminals inside each.
fn worktree_ls(args: &[String]) -> std::io::Result<()> {
    let json = match args.get(3).map(String::as_str) {
        None => false,
        Some("--json") if args.len() == 4 => true,
        _ => return Err(crate::usage(WORKTREE_USAGE)),
    };
    let list = worktrees()?;
    let rows = session_rows()?;
    let roots: Vec<PathBuf> = list.iter().map(|w| real(&w.path)).collect();
    let mut inside: Vec<Vec<&qoro_runtime::control::AgentRow>> = vec![Vec::new(); list.len()];
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
    /// Whether it is a qoro daemon.
    daemon: bool,
}

/// Every process of this user whose directory is inside `root`.
///
/// Read from the processes themselves, never from the index: the index learns a
/// terminal's directory on a throttle, misses an agent a subshell started somewhere else,
/// and knows nothing of a shell outside qoro. A process
/// of another user is not readable and is not ours to protect. qoro's own clients are left
/// out, this one included: an interface or a `wait` started from inside holds no work
/// there. A qoro daemon is kept, because its directory is where every terminal it opens
/// without one starts. Fails closed when the scan cannot even see this process, because
/// an empty answer from a scan that does not work reads exactly like "nobody is inside".
fn processes_inside(root: &std::path::Path) -> std::io::Result<Vec<Occupant>> {
    use qoro_pty::sys;

    let me = std::process::id() as i32;
    let pids = sys::scan_all_pids();
    if !pids.contains(&me) || sys::cwd_of(qoro_pty::Pid(me)).is_none() {
        return Err(std::io::Error::other(
            "cannot list the processes on this machine, so cannot tell whether the worktree \
             is in use; not removing it",
        ));
    }
    Ok(pids
        .into_iter()
        .filter_map(|pid| {
            let cwd = sys::cwd_of(qoro_pty::Pid(pid))?;
            if !real(&cwd).starts_with(root) {
                return None;
            }
            let program = sys::program_of(qoro_pty::Pid(pid))
                .and_then(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "?".to_string());
            let mut daemon = false;
            if program == "qoro" {
                let argv = sys::argv_of(qoro_pty::Pid(pid));
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

/// `qoro worktree rm`: remove a linked worktree, keeping its branch.
///
/// Refused while any process of this user stands inside it, whatever started it; the
/// index only names which of those are qoro terminals. `--force` is for a dirty tree and
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
            "a qoro daemon (pid {pid}) runs from inside {}: stop it or start it elsewhere, \
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
            format!(" (qoro terminal {})", terminals.join(", "))
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
    fn worktree_new_shares_the_new_parse_and_keeps_its_own_flags() {
        let opts = parse_worktree_new_args(&argv(&[
            "qoro", "worktree", "new", "feat/x", "--base", "main", "--space", "work", "--path",
            "../x", "--", "claude", "--resume",
        ]))
        .unwrap();
        assert_eq!(opts.branch, "feat/x");
        assert_eq!(opts.base.as_deref(), Some("main"));
        assert_eq!(opts.path, Some(PathBuf::from("../x")));
        assert_eq!(opts.new.space.as_deref(), Some("work"));
        assert_eq!(opts.new.command, vec!["claude", "--resume"]);
        for bad in [
            &["qoro", "worktree", "new"][..],
            &["qoro", "worktree", "new", "-b"],
            &["qoro", "worktree", "new", "x", "--base", "-evil"],
            &["qoro", "worktree", "new", "x", "--cwd", "/tmp"],
            &["qoro", "worktree", "new", "x", "--json"],
            &["qoro", "worktree", "new", "x", "--bogus"],
        ] {
            assert!(
                parse_worktree_new_args(&argv(bad)).is_err(),
                "{bad:?} parsed"
            );
        }
    }
}

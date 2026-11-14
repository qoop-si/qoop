//! `qoop worktree`: a git worktree beside the repository, with a terminal in it.
//!
//! Git is only ever run with an argv, never through a shell, and every path is compared
//! by component after resolving symlinks, so `/tmp` and `/private/tmp` are one place and
//! `/repo-wt-a` is not inside `/repo`.

use std::path::PathBuf;
use std::process::Command;

use qoop_runtime::control::{ControlRequest, ControlResponse, WorkspaceRow};
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

/// One question to this machine's daemon, or `None` when qoop is not running.
fn ask(request: &ControlRequest) -> std::io::Result<Option<ControlResponse>> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        return Ok(None);
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::ask_daemon(&socket, "qoop-worktree", request))
        .map(Some)
}

/// This machine's rows, or none when qoop is not running: with no daemon there are no
/// terminals, which is the true answer and not a failure.
fn session_rows() -> std::io::Result<Vec<qoop_runtime::control::AgentRow>> {
    match ask(&ControlRequest::ListAgents {
        id: crate::request_id("worktree"),
    })? {
        None => Ok(Vec::new()),
        Some(ControlResponse::Agents { agents, .. }) => Ok(agents),
        Some(ControlResponse::Failed { reason, .. }) => Err(std::io::Error::other(reason)),
        Some(_) => Err(std::io::Error::other(
            "unexpected answer to the agent listing",
        )),
    }
}

/// This machine's spaces, from the running daemon; refused when nothing runs.
fn space_rows() -> std::io::Result<Vec<WorkspaceRow>> {
    match ask(&ControlRequest::ListWorkspaces {
        id: crate::request_id("worktree"),
    })? {
        None => Err(not_awake()),
        Some(ControlResponse::Workspaces { workspaces, .. }) => Ok(workspaces),
        Some(ControlResponse::Failed { reason, .. }) => Err(std::io::Error::other(reason)),
        Some(_) => Err(std::io::Error::other(
            "unexpected answer to the space listing",
        )),
    }
}

fn not_awake() -> std::io::Error {
    std::io::Error::other("nothing is awake: `qoop` or `qoop wake` starts it")
}

/// The space a worktree belongs to: its tab opens there and its settings are that space's.
#[derive(Debug, PartialEq)]
enum Place {
    /// `name` keys its `[spaces.<name>]` table; `open` is what the tab is opened in: a found
    /// space's id, or `--space` as typed, so a space that does not exist yet is made only
    /// when the tab opens. `dir` is the space's folder, when it has one.
    Space {
        name: String,
        open: String,
        dir: Option<String>,
    },
    /// No space, or a tie nothing breaks: the daemon's default space, nothing set up. `tied`
    /// names the spaces of a tie, as `(id, name)`.
    Default { tied: Vec<(i64, String)> },
}

/// Which space a worktree of the repository whose main checkout is `main` belongs to.
///
/// `--space` (`wanted`) is looked up and never made: a number that is no id and no name is
/// refused, a new name is the key as typed. Without it, the space whose folder is the main
/// checkout, else the one with the deepest folder holding it (a folder of several
/// repositories); qass's own space never counts. Spaces that tie there are one when they
/// share a name, and otherwise the one whose name `has_table` wins, when exactly one does.
fn place_of(
    spaces: &[WorkspaceRow],
    main: &std::path::Path,
    wanted: Option<&str>,
    has_table: impl Fn(&str) -> bool,
) -> std::io::Result<Place> {
    if let Some(wanted) = wanted {
        let found = supervise::pick_space(spaces, wanted)?
            .and_then(|id| spaces.iter().find(|s| s.id == id));
        return Ok(Place::Space {
            name: found.map_or(wanted, |s| s.name.as_str()).to_string(),
            open: wanted.to_string(),
            dir: found.and_then(|s| s.dir.clone()),
        });
    }
    let main = real(main);
    let homes: Vec<(&WorkspaceRow, usize)> = spaces
        .iter()
        .filter(|s| s.name != qoop_runtime::asks::QASS_SPACE)
        .filter_map(|s| {
            let dir = real(std::path::Path::new(s.dir.as_deref()?));
            (dir.is_absolute() && main.starts_with(&dir)).then(|| (s, dir.components().count()))
        })
        .collect();
    // The folder equal to the main checkout is the deepest that holds it.
    let deepest = homes.iter().map(|(_, depth)| *depth).max();
    let tied: Vec<&WorkspaceRow> = homes
        .iter()
        .filter(|(_, depth)| Some(*depth) == deepest)
        .map(|(s, _)| *s)
        .collect();
    let mut names: Vec<&str> = tied.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let chosen = match names.as_slice() {
        [one] => Some(*one),
        many => match many
            .iter()
            .filter(|n| has_table(n))
            .collect::<Vec<_>>()
            .as_slice()
        {
            [one] => Some(**one),
            _ => None,
        },
    };
    let Some(name) = chosen else {
        return Ok(Place::Default {
            tied: tied.iter().map(|s| (s.id, s.name.clone())).collect(),
        });
    };
    // Spaces of one name share its table; the tab goes to the lowest id among them.
    let space = tied
        .iter()
        .filter(|s| s.name == name)
        .min_by_key(|s| s.id)
        .expect("a chosen name is a tied space's");
    Ok(Place::Space {
        name: name.to_string(),
        open: space.id.to_string(),
        dir: space.dir.clone(),
    })
}

/// A linked worktree's git common dir and its id there: the `<id>` of
/// `<common>/worktrees/<id>`.
fn git_dirs(tree: &std::path::Path) -> std::io::Result<(PathBuf, String)> {
    let out = git(
        tree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--git-dir",
        ]
        .map(std::ffi::OsStr::new),
    )?;
    let mut lines = out.lines();
    let common = PathBuf::from(lines.next().unwrap_or_default());
    let id = std::path::Path::new(lines.next().unwrap_or_default())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok((common, id))
}

/// The worktree's index (`QOOP_WORKTREE_INDEX`): the main checkout is 0, a linked worktree
/// takes the lowest free number from 1, for good.
///
/// A claim is the file `<common>/qoop-worktree-index/<n>` holding the worktree's id. A claim
/// already holding this id is its index: git reuses an id (the folder's name), so a branch
/// made again takes up the claim its earlier worktree left. Else the lowest number with no
/// claim, or with one whose `<common>/worktrees/<id>` is gone, so a worktree removed by
/// plain git frees its number too. A claim that cannot be read counts as taken. Read and
/// written under a lock of its own, so two parallel `new`s never take one number.
fn claim_index(common: &std::path::Path, id: &str) -> std::io::Result<u32> {
    let claims = common.join("qoop-worktree-index");
    let _held = qoop_runtime::lock::StartupLock::acquire(&common.join("qoop-worktree-index.lock"))?;
    std::fs::create_dir_all(&claims)?;
    let mut held = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(&claims)? {
        let entry = entry?;
        if let Some(n) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|&n| n > 0)
        {
            let holder = std::fs::read_to_string(entry.path()).ok();
            held.insert(n, holder.map(|h| h.trim().to_string()));
        }
    }
    if let Some((&n, _)) = held
        .iter()
        .find(|(_, holder)| holder.as_deref() == Some(id))
    {
        return Ok(n);
    }
    // Unread is taken: a number whose holder nobody can name is never handed out again.
    let free = |holder: &Option<String>| {
        holder
            .as_deref()
            .is_some_and(|h| h.is_empty() || !common.join("worktrees").join(h).is_dir())
    };
    let n = (1..)
        .find(|n| held.get(n).is_none_or(free))
        .expect("a free number");
    std::fs::write(claims.join(n.to_string()), id)?;
    Ok(n)
}

/// Whether a space's name has a `[spaces.<name>.worktree]` table, for breaking a tie.
fn has_table(read: &qoop_tui::config::Read) -> impl Fn(&str) -> bool + '_ {
    |name| read.spaces.get(name).is_some_and(|s| s.worktree)
}

/// What a worktree inherits from a git hook or a script that ran qoop, and would point git
/// or the setup at another repository or change how a pattern matches.
const GIT_SCOPE: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_LITERAL_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Everything after `git worktree add`, for `new` and `setup` alike: claim the worktree's
/// index, then, unless `skip`, set it up as its space says. Prints the `setup=` line,
/// `failed` included; the error is why, for the caller's retry hint.
fn prepare(
    main: &std::path::Path,
    tree: &std::path::Path,
    branch: &str,
    place: &Place,
    read: &qoop_tui::config::Read,
    skip: bool,
) -> Result<(), String> {
    let state = git_dirs(tree)
        .and_then(|(common, id)| claim_index(&common, &id))
        .map_err(|e| {
            eprintln!("setup: no index for {}: {e}", tree.display());
            "no index".to_string()
        })
        .and_then(|index| match skip {
            true => Ok("skipped"),
            false => set_up(main, tree, branch, index, place, read),
        });
    println!("setup={}", state.as_deref().unwrap_or("failed"));
    state.map(|_| ())
}

/// Copy and run what the worktree's space says in `config.toml`: `done` when something was
/// copied or run, `none` when nothing was (no space, none of its settings, nothing matched).
/// A failure says why on stderr and is the error returned.
///
/// The settings are the person's own file and never the repository's, so a cloned
/// repository can never run anything. A table with any problem in it is used not at all,
/// and so is a file whose `[spaces]` did not read whole wherever a space was found or a tie
/// broken: a copy or a command without the part that was wrong is not what was written.
fn set_up(
    main: &std::path::Path,
    tree: &std::path::Path,
    branch: &str,
    index: u32,
    place: &Place,
    read: &qoop_tui::config::Read,
) -> Result<&'static str, String> {
    let Some(space) = settings_of(main, place, read)? else {
        return Ok("none");
    };
    let copied = copy_ignored(main, tree, &space.copy)?;
    if let (Some(line), Place::Space { name, dir, .. }) = (&space.setup, place) {
        let from = dir
            .as_deref()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        eprintln!("setup: space {name}{from}: running {line}");
        run_setup(line, main, tree, branch, index)?;
    }
    Ok(if copied > 0 || space.setup.is_some() {
        "done"
    } else {
        "none"
    })
}

/// The settings that apply to a worktree of `place`, `None` when there are none.
fn settings_of<'a>(
    main: &std::path::Path,
    place: &Place,
    read: &'a qoop_tui::config::Read,
) -> Result<Option<&'a qoop_tui::config::SpaceSetup>, String> {
    let broken = || {
        for problem in &read.problems {
            eprintln!("setup: {} {}", read.path.display(), problem.short());
        }
        Err("config.toml has a problem".to_string())
    };
    let name = match place {
        Place::Default { tied } if tied.len() > 1 && read.broken => return broken(),
        // Said only to a person who set something up, so nobody else sees anything new.
        Place::Default { .. } if read.spaces.is_empty() => return Ok(None),
        Place::Default { tied } if tied.is_empty() => {
            eprintln!(
                "setup: no space has {main} as its folder; qoop space dir NAME {main} sets one",
                main = main.display()
            );
            return Ok(None);
        }
        Place::Default { tied } => {
            let named: Vec<String> = tied
                .iter()
                .map(|(id, name)| format!("{name} ({id})"))
                .collect();
            eprintln!(
                "setup: spaces {} tie for {}: qoop space dir moves one, --space NAME picks one \
                 for this run",
                named.join(", "),
                main.display()
            );
            return Ok(None);
        }
        Place::Space { .. } if read.broken => return broken(),
        Place::Space { name, .. } => name,
    };
    let Some(space) = read.spaces.get(name) else {
        return Ok(None);
    };
    if !space.problems.is_empty() {
        for problem in &space.problems {
            eprintln!("setup: {} {}", read.path.display(), problem.short());
        }
        return Err("config.toml has a problem".to_string());
    }
    Ok(space.worktree.then_some(space))
}

/// Copy the main checkout's ignored files that `patterns` match into `tree`, never over
/// anything already there, and say what was copied.
///
/// Git does the matching (`ls-files --others --ignored`), so a tracked file is never
/// copied and a symlinked folder is never followed. A file this will not copy (its folder
/// outside the main checkout, a part of its place in the worktree that is a symlink or
/// leads outside it, anything but a file or a symlink) stops the copy: it fails closed and
/// says which file and why, after saying what it had copied. How many it copied.
fn copy_ignored(
    main: &std::path::Path,
    tree: &std::path::Path,
    patterns: &[String],
) -> Result<usize, String> {
    use std::os::unix::ffi::OsStrExt;

    if patterns.is_empty() {
        return Ok(0);
    }
    let mut ls = Command::new("git");
    ls.arg("-C")
        .arg(main)
        .args([
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--",
        ])
        .args(patterns.iter().map(|p| format!(":(glob){p}")));
    for var in GIT_SCOPE {
        ls.env_remove(var);
    }
    let out = ls.output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let why = format!(
            "git ls-files: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        eprintln!("setup: {why}");
        return Err(why);
    }
    let (main_real, tree_real) = (real(main), real(tree));
    let mut copied = Vec::new();
    let mut kept = 0;
    let said = |copied: &[String], kept: usize| {
        let files = if copied.len() == 1 { "file" } else { "files" };
        let mut line = format!(
            "setup: copied {} {files} from {}",
            copied.len(),
            main.display()
        );
        if !copied.is_empty() {
            line.push_str(&format!(" ({})", copied.join(", ")));
        }
        if kept > 0 {
            line.push_str(&format!(", kept {kept} already there"));
        }
        eprintln!("{line}");
    };
    // A folder git does not go into (a repository of its own inside) ends in `/`: none of
    // its files are the main checkout's to copy.
    for rel in out
        .stdout
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty() && !r.ends_with(b"/"))
    {
        let rel = std::path::Path::new(std::ffi::OsStr::from_bytes(rel));
        match copy_one(&main_real, &tree_real, rel) {
            Ok(true) => copied.push(rel.display().to_string()),
            Ok(false) => kept += 1,
            Err(why) => {
                said(&copied, kept);
                eprintln!("setup: not copying {}: {why}", rel.display());
                return Err(format!("{} not copied", rel.display()));
            }
        }
    }
    said(&copied, kept);
    Ok(copied.len())
}

/// Copy one file, `rel` under both, from the main checkout into the worktree. `false` when
/// something is already at its place, which is kept. Both roots are resolved.
fn copy_one(
    main: &std::path::Path,
    tree: &std::path::Path,
    rel: &std::path::Path,
) -> Result<bool, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let source = main.join(rel);
    if !real(source.parent().unwrap_or(main)).starts_with(main) {
        return Err(format!("its folder is outside {}", main.display()));
    }
    // The source itself is never followed: a symlink is copied as one. Anything else that
    // is not a file is refused before a folder is made for it.
    let meta = std::fs::symlink_metadata(&source).map_err(|e| e.to_string())?;
    if !meta.is_file() && !meta.file_type().is_symlink() {
        return Err("it is not a file or a symlink".to_string());
    }
    // Every part of its place that already exists, before any folder is made: a symlink
    // there would put the file, and the folders made for it, wherever it points.
    let mut at = tree.to_path_buf();
    for part in rel
        .parent()
        .into_iter()
        .flat_map(std::path::Path::components)
    {
        at.push(part);
        let shown = || at.strip_prefix(tree).unwrap_or(&at).display().to_string();
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("{} in the worktree is a symlink", shown()));
            }
            Ok(m) if !m.is_dir() => {
                return Err(format!("{} in the worktree is not a folder", shown()));
            }
            Ok(_) if !real(&at).starts_with(tree) => {
                return Err(format!("{} leads outside the worktree", shown()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    let target = tree.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let made = if meta.file_type().is_symlink() {
        std::fs::read_link(&source)
            .and_then(|link| std::os::unix::fs::symlink(link, &target))
            .map(|()| true)
    } else {
        // Opened without following a link or waiting on a pipe, and checked on the handle:
        // what was looked at above may have been swapped since. `create_new` is the "never
        // overwrite" in one step: anything there, a symlink included, is kept.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&source)
            .and_then(|mut from| {
                if !from.metadata()?.is_file() {
                    return Err(std::io::Error::other("it is not a file"));
                }
                let to = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(meta.permissions().mode() & 0o777)
                    .open(&target);
                let mut to = match to {
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
                    other => other?,
                };
                let written = std::io::copy(&mut from, &mut to)
                    .and_then(|_| to.set_permissions(meta.permissions()));
                if written.is_err() {
                    let _ = std::fs::remove_file(&target);
                }
                written.map(|()| true)
            })
    };
    match made {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        other => other.map_err(|e| e.to_string()),
    }
}

/// Run a space's `setup` line with `sh -c` in the new worktree. Its output goes to stderr,
/// so stdout keeps only the lines a script reads; it reads nothing, and has no time limit
/// (Ctrl-C reaches it).
fn run_setup(
    line: &str,
    main: &std::path::Path,
    tree: &std::path::Path,
    branch: &str,
    index: u32,
) -> Result<(), String> {
    use std::os::fd::AsFd;

    let to_stderr = std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| e.to_string())?;
    let mut sh = Command::new("sh");
    for var in GIT_SCOPE {
        sh.env_remove(var);
    }
    let status = sh
        .arg("-c")
        .arg(line)
        .current_dir(tree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(to_stderr))
        .env("QOOP_WORKTREE", tree)
        .env("QOOP_WORKTREE_SOURCE", main)
        .env("QOOP_WORKTREE_BRANCH", branch)
        .env("QOOP_WORKTREE_INDEX", index.to_string())
        .status()
        .map_err(|e| format!("sh: {e}"))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(format!("exit {code}")),
        None => Err("ended by a signal".to_string()),
    }
}

/// `qoop worktree setup`: set up a linked worktree that exists already (the caller's own
/// when none is named) as `new` does: claim its index, copy what is missing, run `setup`.
/// For a retry after a failure, or a worktree made by plain `git worktree add`. Opens no
/// terminal, so it never makes a space either: `--space` with a new name is only the key.
fn worktree_setup(args: &[String]) -> std::io::Result<()> {
    let mut target = None;
    let mut space = None;
    let mut it = args.iter().skip(3);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--space" => {
                space = Some(it.next().cloned().ok_or_else(|| {
                    crate::usage(format!("missing value for --space; {WORKTREE_USAGE}"))
                })?);
            }
            other if other.starts_with('-') || target.is_some() => {
                return Err(crate::usage(format!(
                    "unexpected {other}; {WORKTREE_USAGE}"
                )));
            }
            other => target = Some(other.to_string()),
        }
    }
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    if std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_err() {
        return Err(not_awake());
    }
    let list = worktrees()?;
    let at = match &target {
        Some(target) => find_worktree(&list, target)?,
        None => {
            let roots: Vec<PathBuf> = list.iter().map(|w| real(&w.path)).collect();
            worktree_of(&roots, &std::env::current_dir()?)
                .ok_or_else(|| std::io::Error::other("this is not inside a worktree"))?
        }
    };
    if at == 0 {
        return Err(std::io::Error::other(
            "that is the main checkout: it is where the files come from",
        ));
    }
    let tree = &list[at];
    if !tree.path.exists() {
        return Err(std::io::Error::other(format!(
            "{} is already gone; `git worktree prune` forgets it",
            tree.path.display()
        )));
    }
    let main = &list[0].path;
    let read = qoop_tui::config::read();
    let place = place_of(&space_rows()?, main, space.as_deref(), has_table(&read))?;
    let path = real(&tree.path);
    let branch = tree.branch.clone().unwrap_or_default();
    prepare(main, &path, &branch, &place, &read, false).map_err(|why| {
        let again = tree
            .branch
            .clone()
            .unwrap_or_else(|| path.display().to_string());
        std::io::Error::other(format!(
            "setup failed ({why}); fix it, then qoop worktree setup {again}"
        ))
    })
}

/// A worktree by its branch, else by its path from here.
fn find_worktree(list: &[Worktree], target: &str) -> std::io::Result<usize> {
    let wanted = real(&std::env::current_dir()?.join(target));
    list.iter()
        .position(|w| w.branch.as_deref() == Some(target))
        .or_else(|| list.iter().position(|w| real(&w.path) == wanted))
        .ok_or_else(|| std::io::Error::other(format!("no worktree for branch or path {target}")))
}

/// `qoop worktree`: a git worktree beside the repository, with a terminal in it.
pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("new") => worktree_new(args),
        Some("ls") => worktree_ls(args),
        Some("rm") => worktree_rm(args),
        Some("setup") => worktree_setup(args),
        Some("help" | "--help" | "-h") => {
            println!("{WORKTREE_USAGE}");
            Ok(())
        }
        _ => Err(crate::usage(WORKTREE_USAGE)),
    }
}

const WORKTREE_USAGE: &str = "usage:\n\
     \x20 qoop worktree new <branch> [--base REF] [--path DIR] [--space ID|NAME] [--name NAME] [--no-setup] [-- CMD...]\n\
     \x20 qoop worktree setup [<branch|path>] [--space ID|NAME]\n\
     \x20 qoop worktree ls [--json]\n\
     \x20 qoop worktree rm <branch|path> [--force]";

/// Parsed `qoop worktree new` arguments. Its own flags come out first and the rest is
/// `qoop new`'s own parse, so the two cannot drift apart about `--space` or `--`.
struct WorktreeNewOpts {
    branch: String,
    base: Option<String>,
    path: Option<PathBuf>,
    no_setup: bool,
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
    let mut no_setup = false;
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
            "--no-setup" => no_setup = true,
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
        no_setup,
        new,
    })
}

/// `qoop worktree new`: add the worktree, then open a terminal in it named after the
/// branch. A branch already checked out elsewhere is refused with where, because git's
/// own refusal is about a path the person did not type. A branch that exists only on a
/// remote is handed to git by name, so git makes the local branch tracking it rather than
/// a fresh one from HEAD. The terminal opens in the worktree's space (see [`place_of`]);
/// before it does, the worktree claims its index and that space's setup runs (see
/// [`set_up`]). A setup that fails keeps the worktree and opens no terminal.
fn worktree_new(args: &[String]) -> std::io::Result<()> {
    let WorktreeNewOpts {
        branch,
        base,
        path,
        no_setup,
        mut new,
    } = parse_worktree_new_args(args).map_err(crate::usage)?;
    // Checked before git runs, so qoop not running does not leave a worktree behind.
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    if std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_err() {
        return Err(not_awake());
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
    // Found before git runs and never made here, so a bad `--space` leaves no worktree.
    let read = qoop_tui::config::read();
    let place = place_of(
        &space_rows()?,
        &main,
        new.space.as_deref(),
        has_table(&read),
    )?;
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
    prepare(&main, &path, &branch, &place, &read, no_setup).map_err(|why| {
        std::io::Error::other(format!(
            "setup failed ({why}); fix it, then qoop worktree setup {branch}"
        ))
    })?;
    new.space = match place {
        Place::Space { open, .. } => Some(open),
        Place::Default { .. } => None,
    };
    new.cwd = Some(path);
    new.name = new.name.or_else(|| Some(branch.clone()));
    new.spawned = true;
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
    let at = find_worktree(&list, target)?;
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
            "git@github.com:pat/app.git",
            "git@github.com:pat/app",
            "ssh://git@github.com/pat/app.git",
            "ssh://git@github.com:22/pat/app/",
            "https://github.com/pat/app",
            "https://github.com/pat/app.git/",
            "https://pat:token@github.com/pat/app.git",
            "https://GitHub.com/PAT/app",
        ] {
            assert!(
                owned(url, "Pat").is_some_and(|r| r.eq_ignore_ascii_case("pat/app")),
                "{url}"
            );
        }
        assert_eq!(
            owned("git@github.com:pat/my.site.git", "pat").as_deref(),
            Some("pat/my.site")
        );
        for (url, login) in [
            ("git@github.com:bigco/app.git", "pat"),
            ("git@github.example.com:pat/x.git", "pat"),
            ("https://github.com.evil.io/pat/x", "pat"),
            ("https://gitlab.com/pat/x", "pat"),
            ("git@github.com:pat/x.git", ""),
            ("git@github.com:pat/x/y.git", "pat"),
            ("git@github.com:pat/.git", "pat"),
            ("/src/pat/x", "pat"),
            ("./a/b:pat/x", "pat"),
            ("https://evil.com#@github.com/pat/x", "pat"),
            ("https://evil.com?@github.com/pat/x", "pat"),
            ("https://evil.com#/@github.com/pat/x", "pat"),
            ("https://evil.com\\@github.com/pat/x", "pat"),
            ("https://evil.com @github.com/pat/x", "pat"),
            ("https://evil.com\t@github.com/pat/x", "pat"),
            ("https://github.com/pat/x?y=1", "pat"),
            ("https://github.com/pat/x#frag", "pat"),
            ("", "pat"),
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
        std::fs::write(&gh, "#!/bin/sh\necho Pat\n").expect("gh");
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
        std::fs::write(&gh, "#!/bin/sh\necho pat\nexit 1\n").expect("gh");
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
        std::fs::write(&gh, "#!/bin/sh\necho pat\n").expect("gh");
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

    fn space(id: i64, name: &str, dir: Option<&str>) -> WorkspaceRow {
        WorkspaceRow {
            id,
            name: name.into(),
            terminals: 0,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            keep_awake: false,
            qass_skip: false,
            goal: None,
            dir: dir.map(String::from),
        }
    }

    /// The place of the space `open` names, its folder taken from `spaces`.
    fn found(spaces: &[WorkspaceRow], name: &str, open: &str) -> Place {
        Place::Space {
            name: name.into(),
            open: open.into(),
            dir: spaces
                .iter()
                .find(|s| s.id.to_string() == open)
                .and_then(|s| s.dir.clone()),
        }
    }

    #[test]
    fn a_worktree_belongs_to_the_space_of_its_repo_or_the_deepest_folder_holding_it() {
        let main = Path::new("/nowhere/code/app");
        let none = |_: &str| false;
        let spaces = vec![
            space(1, "default", None),
            space(2, "code", Some("/nowhere/code")),
            space(3, "app", Some("/nowhere/code/app")),
            space(4, "qass", Some("/nowhere/code/app")),
            space(5, "sibling", Some("/nowhere/code/app-wt-x")),
        ];
        assert_eq!(
            place_of(&spaces, main, None, none).unwrap(),
            found(&spaces, "app", "3")
        );
        // No exact folder: the deepest that holds it; qass's own space never counts.
        assert_eq!(
            place_of(&spaces[..3], Path::new("/nowhere/code/web"), None, none).unwrap(),
            found(&spaces, "code", "2")
        );
        assert_eq!(
            place_of(&spaces[3..], main, None, none).unwrap(),
            Place::Default { tied: vec![] }
        );
        // `--space` is looked up, never made: an id gives its name, a new name is the key,
        // a number that is neither is refused.
        assert_eq!(
            place_of(&spaces, main, Some("2"), none).unwrap(),
            found(&spaces, "code", "2")
        );
        assert_eq!(
            place_of(&spaces, main, Some("fresh"), none).unwrap(),
            found(&spaces, "fresh", "fresh")
        );
        assert!(place_of(&spaces, main, Some("99"), none).is_err());
    }

    #[test]
    fn a_tie_goes_to_the_one_name_with_a_table_and_one_name_is_the_lowest_id() {
        let main = Path::new("/nowhere/app");
        let spaces = vec![
            space(2, "web", Some("/nowhere/app")),
            space(3, "app", Some("/nowhere/app/")),
        ];
        assert_eq!(
            place_of(&spaces, main, None, |n| n == "app").unwrap(),
            found(&spaces, "app", "3")
        );
        for tables in [&[][..], &["app", "web"]] {
            assert_eq!(
                place_of(&spaces, main, None, |n| tables.contains(&n)).unwrap(),
                Place::Default {
                    tied: vec![(2, "web".into()), (3, "app".into())]
                },
                "{tables:?}"
            );
        }
        let same = vec![
            space(7, "app", Some("/nowhere/app")),
            space(4, "app", Some("/nowhere/app")),
            space(5, "app", Some("/elsewhere")),
        ];
        assert_eq!(
            place_of(&same, main, None, |_| false).unwrap(),
            found(&same, "app", "4")
        );
        // Of two spaces called alike, the one whose folder it is, by id.
        assert_eq!(
            place_of(&same[1..], Path::new("/elsewhere"), None, |_| false).unwrap(),
            found(&same, "app", "5")
        );
    }

    #[test]
    fn an_index_is_the_lowest_free_number_and_a_reused_id_keeps_its_own() {
        let dir = tempfile::tempdir().expect("dir");
        let common = dir.path();
        for id in ["a", "b", "c"] {
            std::fs::create_dir_all(common.join("worktrees").join(id)).unwrap();
        }
        assert_eq!(claim_index(common, "a").unwrap(), 1);
        assert_eq!(claim_index(common, "b").unwrap(), 2);
        assert_eq!(claim_index(common, "a").unwrap(), 1, "asked again");
        // `b` went away without qoop: its number is free for the next one.
        std::fs::remove_dir(common.join("worktrees/b")).unwrap();
        assert_eq!(claim_index(common, "c").unwrap(), 2);
        // A claim left by an earlier worktree of the same id is taken up, never doubled.
        std::fs::write(common.join("qoop-worktree-index/5"), "d").unwrap();
        std::fs::create_dir_all(common.join("worktrees/d")).unwrap();
        assert_eq!(claim_index(common, "d").unwrap(), 5);
        let holding_d = std::fs::read_dir(common.join("qoop-worktree-index"))
            .unwrap()
            .filter(|e| std::fs::read_to_string(e.as_ref().unwrap().path()).unwrap() == "d")
            .count();
        assert_eq!(holding_d, 1);
        // A claim that cannot be read is taken: never handed out again.
        std::fs::remove_file(common.join("qoop-worktree-index/2")).unwrap();
        std::fs::create_dir(common.join("qoop-worktree-index/2")).unwrap();
        std::fs::create_dir_all(common.join("worktrees/e")).unwrap();
        assert_eq!(claim_index(common, "e").unwrap(), 3);
    }

    #[test]
    fn parallel_claims_take_distinct_numbers() {
        let dir = tempfile::tempdir().expect("dir");
        let common = dir.path();
        let ids: Vec<String> = (0..8).map(|i| format!("w{i}")).collect();
        for id in &ids {
            std::fs::create_dir_all(common.join("worktrees").join(id)).unwrap();
        }
        let mut got: Vec<u32> = std::thread::scope(|scope| {
            let runs: Vec<_> = ids
                .iter()
                .map(|id| scope.spawn(move || claim_index(common, id).unwrap()))
                .collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        got.sort_unstable();
        assert_eq!(got, (1..=8).collect::<Vec<u32>>());
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
}

//! `qoop task verify <KEY|id>`: the closing facts of a finished task, worked out here so qass
//! does no arithmetic. Facts only: it never closes or writes anything. The PR its evidence
//! names merged into the default branch, after the task opened and from the worker's own
//! worktree; that worktree gone; the approved install newer than the merge. At most 200
//! words, cut here; never a screen's or a transcript's text.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use qoop_runtime::control::THIS_MACHINE;
use qoop_runtime::paths;

use crate::qass::json;
use crate::qass_brief::{Task, cap, clock, cut, task_of};
use crate::worktree::{git, parse_worktrees, repo_of, worktree_slug};

const WORDS: usize = 200;
/// The most words the `Missing:` line itself takes, so the whole stays under `WORDS`.
const MISSING_WORDS: usize = 150;
const USAGE: &str = "usage: qoop task verify <KEY|id> [--repo <path>] [--json]";
const PASSED: &str = "Missing: none; still yours: Verify where it runs, then close";

/// A PR the evidence names: its `owner/repo` when the text says one, and its number.
#[derive(Clone, Debug, PartialEq)]
struct Named {
    repo: Option<String>,
    number: u64,
}

impl Named {
    fn label(&self) -> String {
        match &self.repo {
            Some(repo) => format!("{repo}#{}", self.number),
            None => format!("PR #{}", self.number),
        }
    }
}

/// What gh says of one PR, times in seconds.
#[derive(Clone, Debug, PartialEq)]
struct Pr {
    base: String,
    head: String,
    created: Option<i64>,
    merged: Option<i64>,
    /// The open PRs from the same head branch, or why gh could not say.
    open: Result<Vec<u64>, String>,
}

/// The repository a task is in, as git, gh and the approved copy say.
#[derive(Clone, Debug)]
struct Repo {
    root: PathBuf,
    /// The `<slug>` of the worker's `<repo>-wt-<slug>` folder; `None` for the main checkout
    /// or a folder named otherwise.
    slug: Option<String>,
    /// `owner/repo` and its default branch, or why gh could not say.
    gh: Result<(String, String), String>,
    /// The branches of its listed worktrees, the main one left out.
    branches: Vec<String>,
    /// The approved copy's `installed <path>` and its mtime (`None`: not there), or why
    /// there is nothing to compare.
    install: Result<(PathBuf, Option<i64>), String>,
}

/// What gh says of one PR named, in the repo it was asked of: that repo's `owner/repo`,
/// default branch and listed worktree branches, and whether it is the worker's own repo.
#[derive(Clone, Debug)]
struct Asked {
    repo: String,
    default: String,
    branches: Vec<String>,
    own: bool,
    pr: Pr,
}

/// Why a task has no repo to judge it by.
#[derive(Clone, Debug, Default, PartialEq)]
enum Folder {
    /// No folder: no live row, none kept on its record, no `--repo`.
    #[default]
    Unknown,
    Gone(PathBuf),
    /// `/`, the home folder or a folder holding repos: where an agent starts, not where it
    /// works.
    NoRepo(PathBuf),
    /// A folder in no git repo: work with no PR to show.
    NonGit(PathBuf),
}

/// One task to verify.
#[derive(Clone, Debug, Default)]
struct Work {
    id: i64,
    /// An agent on a remote: its repo is there.
    remote: Option<String>,
    opened: i64,
    /// Every evidence body, newest first.
    evidence: Vec<String>,
    repo: Option<Repo>,
    /// Each PR named and what gh said of it; none without a repo.
    prs: Vec<(Named, Result<Asked, String>)>,
    /// Why there is no repo, when there is none.
    place: Folder,
}

/// Every task the target covers: an item's linked tasks, or the one task. `Err`: its id,
/// when it could not be read.
#[derive(Clone, Debug, Default)]
struct Facts {
    target: String,
    works: Vec<Result<Work, i64>>,
}

/// The PRs a text names: `PR #N`, `pull #N`, `owner/repo#N` or a github.com pull URL. A bare
/// `#N` is not one (an issue, a step, a count).
fn named_prs<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<Named> {
    let number = |n: &str| n.parse::<u64>().ok().filter(|n| *n > 0);
    let part = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    let mut out = Vec::new();
    for text in texts {
        let words: Vec<String> = text
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| "()[]{}<>,.;:!?'\"`*".contains(c))
                    .to_ascii_lowercase()
            })
            .collect();
        for (i, w) in words.iter().enumerate() {
            let bare = w.split_once("://").map_or(w.as_str(), |(_, r)| r);
            let url = bare
                .strip_prefix("www.")
                .unwrap_or(bare)
                .strip_prefix("github.com/");
            let found = if let Some(rest) = url {
                let mut p = rest.split(['/', '#', '?']);
                match (p.next(), p.next(), p.next(), p.next()) {
                    (Some(o), Some(r), Some("pull"), Some(n)) if part(o) && part(r) => number(n)
                        .map(|number| Named {
                            repo: Some(format!("{o}/{r}")),
                            number,
                        }),
                    _ => None,
                }
            } else if w == "pr" || w == "pull" {
                words
                    .get(i + 1)
                    .and_then(|n| n.strip_prefix('#'))
                    .and_then(number)
                    .map(|number| Named { repo: None, number })
            } else if let Some(n) = w.strip_prefix("pr#") {
                number(n).map(|number| Named { repo: None, number })
            } else if let Some((repo, n)) = w.split_once('#')
                && let Some((o, r)) = repo.split_once('/')
                && part(o)
                && part(r)
            {
                number(n).map(|number| Named {
                    repo: Some(repo.to_string()),
                    number,
                })
            } else {
                None
            };
            if let Some(found) = found
                && !out.contains(&found)
            {
                out.push(found);
            }
        }
    }
    out
}

/// The main checkout and the slug of the `<parent>/<repo>-wt-<slug>` folder `dir` is in (or
/// is): at the first `-wt-` whose prefix `is_repo` takes, so a slug may hold `-wt-` too.
fn tied(dir: &Path, is_repo: impl Fn(&Path) -> bool) -> Option<(PathBuf, String)> {
    dir.ancestors().find_map(|folder| {
        let name = folder.file_name()?.to_str()?;
        let parent = folder.parent()?;
        name.match_indices("-wt-").find_map(|(at, _)| {
            let main = parent.join(&name[..at]);
            (at > 0 && is_repo(&main)).then(|| (main, name[at + 4..].to_string()))
        })
    })
}

fn same(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

fn main_of(dir: &Path) -> Option<PathBuf> {
    repo_of(dir).ok().map(|r| r.root)
}

/// The main checkout of a worker's repo and its worktree's slug, from the first of `first`
/// and `then` that names one (its `-wt-` folder, even when gone): a live folder through git;
/// a folder gone, from its `-wt-` name when the prefix is a main checkout. A main checkout
/// has no slug, so nothing is tied to it.
fn locate(first: Option<&Path>, then: Option<&Path>) -> Option<(PathBuf, Option<String>)> {
    let gone = |m: &Path| m.is_dir() && main_of(m).is_some_and(|r| same(&r, m));
    first.into_iter().chain(then).find_map(|dir| {
        if dir.is_dir()
            && let Some(root) = main_of(dir)
        {
            let slug = tied(dir, |m| same(m, &root)).map(|(_, s)| s);
            return Some((root, slug));
        }
        tied(dir, gone).map(|(root, slug)| (root, Some(slug)))
    })
}

/// gh's JSON answer, found on `path` as a shell finds it; missing or failing is said, never
/// taken for an answer.
fn gh(path: &OsStr, dir: &Path, args: &[&str]) -> Result<serde_json::Value, String> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .output()
        .map_err(|_| "gh not found".to_string())?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(format!("gh failed: {}", cut(&said, 60)));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| "gh gave no JSON".to_string())
}

fn seconds(value: &serde_json::Value) -> Option<i64> {
    qoop_history::rfc3339_to_millis(value.as_str()?).map(|ms| ms / 1000)
}

/// The path of the `- where: installed <path>` line in the `## Verify where it runs` section
/// of an approved copy; any other line naming an install is not where it runs.
fn installed(text: &str) -> Option<String> {
    let mut lines = text.lines();
    lines.find(|l| l.trim() == "## Verify where it runs")?;
    lines
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.trim().strip_prefix("- where:"))
        .find_map(|l| l.trim().trim_matches('`').strip_prefix("installed "))
        .map(|p| {
            p.trim()
                .trim_matches(|c: char| "`'\".;,".contains(c))
                .to_string()
        })
        .filter(|p| !p.is_empty())
}

/// The install the owner approved for this repo, from qass's copy `init-<repo>.md` only:
/// a repo's own workflow file is the worker's text, never where the install is looked up.
fn install_of(root: &Path, data: Option<&Path>) -> Result<(PathBuf, Option<i64>), String> {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned());
    let copy = format!("init-{}.md", name.unwrap_or_default());
    let text = data
        .and_then(|d| std::fs::read_to_string(d.join(&copy)).ok())
        .ok_or(format!("no approved copy {copy}"))?;
    let path = installed(&text).ok_or("the approved Verify section names no installed path")?;
    let path = match path.strip_prefix("~/") {
        Some(rest) => {
            std::env::var_os("HOME").map_or(PathBuf::from(&path), |h| PathBuf::from(h).join(rest))
        }
        None => PathBuf::from(path),
    };
    let mtime = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    Ok((path, mtime))
}

/// The branches of a checkout's listed worktrees, the main one left out.
fn branches_of(root: &Path) -> Vec<String> {
    let list = git(
        root,
        &["worktree", "list", "--porcelain", "-z"].map(OsStr::new),
    )
    .unwrap_or_default();
    parse_worktrees(&list)
        .into_iter()
        .skip(1)
        .filter_map(|w| w.branch)
        .collect()
}

/// Everything about one repo: gh, its worktrees, the approved install.
fn repo_facts(root: PathBuf, slug: Option<String>, path: &OsStr, data: Option<&Path>) -> Repo {
    let gh_repo = gh(
        path,
        &root,
        &["repo", "view", "--json", "nameWithOwner,defaultBranchRef"],
    )
    .and_then(|v| {
        let name = v["nameWithOwner"].as_str().map(str::to_ascii_lowercase);
        let default = v["defaultBranchRef"]["name"].as_str().map(String::from);
        name.zip(default).ok_or("gh named no repo".to_string())
    });
    let branches = branches_of(&root);
    let install = install_of(&root, data);
    Repo {
        root,
        slug,
        gh: gh_repo,
        branches,
        install,
    }
}

/// `owner/repo` of a GitHub remote URL, lower case: `git@github.com:o/r.git`,
/// `https://github.com/o/r` or `ssh://git@github.com/o/r.git`.
fn github_name(url: &str) -> Option<String> {
    let (_, rest) = url.trim().split_once("github.com")?;
    let rest = rest.trim_start_matches([':', '/']);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (o, r) = rest.split_once('/')?;
    (!o.is_empty() && !r.is_empty() && !r.contains('/'))
        .then(|| format!("{o}/{r}").to_ascii_lowercase())
}

/// The main checkout of another repo of a multi-repo task, beside the worker's repo: found
/// by its `origin` remote, never by its folder's name. Two such checkouts are not guessed
/// between: either one's worktrees could be the ones still listed.
fn beside(root: &Path, name: &str) -> Result<PathBuf, String> {
    let origin = ["remote", "get-url", "origin"].map(OsStr::new);
    let found: Vec<PathBuf> = root
        .parent()
        .and_then(|p| std::fs::read_dir(p).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        // A main checkout has a `.git` folder; a worktree's is a file.
        .filter(|d| d.join(".git").is_dir())
        .filter(|d| {
            git(d, &origin)
                .ok()
                .and_then(|u| github_name(&u))
                .as_deref()
                == Some(name)
        })
        .collect();
    match &found[..] {
        [one] => Ok(one.clone()),
        [] => Err(format!("no checkout of {name} beside {}", root.display())),
        _ => Err(format!(
            "{} checkouts of {name} beside {}",
            found.len(),
            root.display()
        )),
    }
}

/// What gh says of PR `n` in `repo`, and the open PRs from its head branch.
fn ask_pr(path: &OsStr, dir: &Path, repo: &str, n: u64) -> Result<Pr, String> {
    let n = n.to_string();
    let fields = "baseRefName,headRefName,createdAt,mergedAt";
    gh(
        path,
        dir,
        &["pr", "view", &n, "--repo", repo, "--json", fields],
    )
    .map(|v| {
        let head = v["headRefName"].as_str().unwrap_or("").to_string();
        let list = ["pr", "list", "--repo", repo, "--head", &head];
        // No head branch: nothing to look up, and never a pass.
        let open = if head.is_empty() {
            Err("no head branch".to_string())
        } else {
            gh(
                path,
                dir,
                &[&list[..], &["--state", "open", "--json", "number"]].concat(),
            )
        }
        .map(|v| {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["number"].as_u64())
                .collect()
        });
        Pr {
            base: v["baseRefName"].as_str().unwrap_or("").to_string(),
            created: seconds(&v["createdAt"]),
            merged: seconds(&v["mergedAt"]),
            head,
            open,
        }
    })
}

/// Each PR the evidence names, asked of its own repo: the worker's, or another one checked
/// out beside it. Nothing is asked when gh cannot name the worker's repo.
fn asked(repo: &Repo, evidence: &[String], path: &OsStr) -> Vec<(Named, Result<Asked, String>)> {
    let Ok((own, own_default)) = &repo.gh else {
        return Vec::new();
    };
    named_prs(evidence.iter().map(String::as_str))
        .into_iter()
        .map(|named| {
            let name = named.repo.clone().unwrap_or_else(|| own.clone());
            let place = if name == *own {
                Ok((
                    repo.root.clone(),
                    own_default.clone(),
                    repo.branches.clone(),
                ))
            } else {
                beside(&repo.root, &name).and_then(|dir| {
                    let v = gh(
                        path,
                        &dir,
                        &["repo", "view", &name, "--json", "defaultBranchRef"],
                    )?;
                    let default = v["defaultBranchRef"]["name"]
                        .as_str()
                        .ok_or("gh named no default branch")?
                        .to_string();
                    let branches = branches_of(&dir);
                    Ok((dir, default, branches))
                })
            };
            let said = place.and_then(|(dir, default, branches)| {
                Ok(Asked {
                    pr: ask_pr(path, &dir, &name, named.number)?,
                    own: name == *own,
                    repo: name,
                    default,
                    branches,
                })
            });
            (named, said)
        })
        .collect()
}

/// Why a folder in no repo leaves its task without one: gone; a place that proves nothing
/// (outside the home folder, the home folder itself, a hidden folder, one holding repos, or
/// one inside a repo git could not read); or non-git work.
fn place_of(dir: &Path, home: Option<&Path>) -> Folder {
    let holds_repos = || {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.path().join(".git").exists())
    };
    let in_repo = || dir.ancestors().any(|a| a.join(".git").exists());
    // Below the home folder, no part hidden: `/tmp` or `~/.cache` is where an agent wanders,
    // not where its work is.
    let mine = home.is_some_and(|h| {
        dir.strip_prefix(h).is_ok_and(|rest| {
            rest.components().next().is_some()
                && rest
                    .components()
                    .all(|c| !c.as_os_str().to_string_lossy().starts_with('.'))
        })
    });
    let dir = dir.to_path_buf();
    if !dir.is_dir() {
        Folder::Gone(dir)
    } else if !mine || holds_repos() || in_repo() {
        Folder::NoRepo(dir)
    } else {
        Folder::NonGit(dir)
    }
}

/// One local task's facts: its repo found from `--repo` (qass's own word for where the work
/// is), else from its folder.
fn work(
    task: &Task,
    cwd: Option<&Path>,
    flag: Option<&Path>,
    path: &OsStr,
    data: Option<&Path>,
    home: Option<&Path>,
) -> Work {
    let repo = locate(flag, cwd).map(|(root, slug)| repo_facts(root, slug, path, data));
    let prs = repo
        .as_ref()
        .map_or_else(Vec::new, |r| asked(r, &task.evidence, path));
    let place = match (&repo, flag.or(cwd)) {
        (None, Some(dir)) => place_of(dir, home),
        _ => Folder::Unknown,
    };
    Work {
        id: task.id,
        remote: None,
        opened: task.opened,
        evidence: task.evidence.clone(),
        repo,
        prs,
        place,
    }
}

/// Why a PR does not pass, or when it merged.
fn verdict(pr: &Pr, opened: i64, default: &str, slug: Option<&str>) -> Result<i64, String> {
    let merged = pr.merged.ok_or("not merged")?;
    if merged < opened {
        return Err("merged before the task opened".into());
    }
    if pr.created.is_none_or(|c| c < opened) {
        return Err("created before the task opened".into());
    }
    if pr.base != default {
        return Err(format!("into {}, not {default}", pr.base));
    }
    match slug {
        None => Err("not tied to this task".into()),
        Some(s) if worktree_slug(&pr.head) != s => {
            Err(format!("from {}, not this worker's worktree", pr.head))
        }
        Some(_) => Ok(merged),
    }
}

/// One task's lines, and what it misses.
fn check(w: &Work, lines: &mut Vec<String>, missing: &mut Vec<String>) {
    let me = format!("#{}", w.id);
    if let Some(remote) = &w.remote {
        missing.push(format!("{me} remote {remote}: ask the owner"));
        return;
    }
    lines.push(format!(
        "{me} evidence: {}; newest: {}",
        w.evidence.len(),
        w.evidence.first().map_or("none".into(), |e| cut(e, 80))
    ));
    let Some(repo) = &w.repo else {
        no_repo(w, &me, lines, missing);
        return;
    };
    let worktree = repo
        .slug
        .as_deref()
        .map_or("not a -wt- worktree".into(), |s| format!("worktree {s}"));
    let (name, default) = match &repo.gh {
        Ok(v) => v,
        Err(why) => {
            lines.push(format!("{me} repo {}, {worktree}", repo.root.display()));
            missing.push(format!("{me} {why}"));
            return;
        }
    };
    lines.push(format!(
        "{me} repo {} ({name}, default {default}), {worktree}",
        repo.root.display()
    ));
    if w.prs.is_empty() {
        missing.push(format!("{me} no PR named in its evidence"));
    }
    let mut newest = None;
    for (named, said) in &w.prs {
        let label = named.label();
        let a = match said {
            Err(why) => {
                missing.push(format!("{me} {label}: {why}"));
                continue;
            }
            Ok(a) => a,
        };
        let pr = &a.pr;
        match verdict(pr, w.opened, &a.default, repo.slug.as_deref()) {
            Err(why) => missing.push(format!("{me} {label} {why}")),
            Ok(at) => {
                lines.push(format!(
                    "{me} {label} merged {} {} UTC from {} into {} of {}",
                    qoop_history::date_utc(at * 1000),
                    clock(at, 0),
                    pr.head,
                    a.default,
                    a.repo
                ));
                if a.branches.contains(&pr.head) {
                    missing.push(format!("{me} worktree of {} still listed", pr.head));
                }
                match &pr.open {
                    Ok(open) if open.is_empty() => {}
                    Ok(open) => {
                        let open: Vec<String> = open.iter().map(|n| format!("#{n}")).collect();
                        missing.push(format!(
                            "{me} open PR {} from {} too",
                            open.join(" "),
                            pr.head
                        ));
                    }
                    Err(why) => missing.push(format!("{me} open PRs from {}: {why}", pr.head)),
                }
                // The install is this repo's: only its own merges are compared with it.
                if a.own {
                    newest = newest.max(Some(at));
                }
            }
        }
    }
    match (&repo.install, newest) {
        (Err(why), _) => lines.push(format!("{me} install: not checked ({why})")),
        (Ok((path, None)), _) => missing.push(format!("{me} install {} not found", path.display())),
        (Ok((path, Some(_))), None) => lines.push(format!(
            "{me} install {}: no merged PR to compare",
            path.display()
        )),
        (Ok((path, Some(at))), Some(merged)) if *at < merged => missing.push(format!(
            "{me} install {} older than the merge",
            path.display()
        )),
        (Ok((path, Some(at))), Some(merged)) => lines.push(format!(
            "{me} install {} {} min newer than the merge",
            path.display(),
            (at - merged) / 60
        )),
    }
}

/// A task with no repo to judge it by: its folder says why. Non-git work passes on its
/// evidence alone, which qass then reads; a PR it names cannot be checked from there.
fn no_repo(w: &Work, me: &str, lines: &mut Vec<String>, missing: &mut Vec<String>) {
    let fix = "pass --repo <its -wt- folder>";
    match &w.place {
        Folder::Unknown => missing.push(format!("{me} no folder recorded; {fix}")),
        Folder::Gone(dir) => missing.push(format!("{me} folder {} is gone", dir.display())),
        Folder::NoRepo(dir) => {
            missing.push(format!("{me} folder {} is no repo; {fix}", dir.display()))
        }
        Folder::NonGit(dir) => {
            let named = named_prs(w.evidence.iter().map(String::as_str));
            if let Some(pr) = named.first() {
                missing.push(format!(
                    "{me} names {} but its folder is no repo; {fix}",
                    pr.label()
                ));
            } else if w.evidence.is_empty() {
                missing.push(format!("{me} no evidence"));
            } else {
                lines.push(format!(
                    "{me} {} is not a git folder: evidence only, read it",
                    dir.display()
                ));
            }
        }
    }
}

/// `Missing: ...` at most `MISSING_WORDS` words, `+N more` saying how many went.
fn missing_line(missing: &[String]) -> String {
    if missing.is_empty() {
        return PASSED.to_string();
    }
    let mut shown = Vec::new();
    let mut words = 1;
    for m in missing {
        let n = m.split_whitespace().count();
        if words + n + 2 > MISSING_WORDS {
            break;
        }
        words += n;
        shown.push(m.as_str());
    }
    let mut line = format!("Missing: {}", shown.join("; "));
    if shown.len() < missing.len() {
        line += &format!("; +{} more", missing.len() - shown.len());
    }
    line
}

/// The lines, and the same as JSON.
fn compose(f: &Facts) -> (Vec<String>, serde_json::Value) {
    let mut lines = vec![format!("verify {}", f.target)];
    let mut missing = Vec::new();
    for w in &f.works {
        match w {
            Ok(w) => check(w, &mut lines, &mut missing),
            Err(id) => missing.push(format!("#{id} unreadable")),
        }
    }
    if f.works.is_empty() {
        missing.push(format!("{} has no task linked", f.target));
    }
    lines.push(missing_line(&missing));
    let lines = cap(lines, WORDS);
    let json = serde_json::json!({
        "target": f.target,
        "tasks": f.works.iter().map(|w| w.as_ref().map_or_else(|id| *id, |w| w.id)).collect::<Vec<_>>(),
        "missing": missing,
        "passed": missing.is_empty(),
        "lines": lines,
    });
    (lines, json)
}

/// The tasks a `task show --json` page covers: an item's linked tasks still open (a remote
/// one by its machine; one closed was checked when it closed, before the item), or the one
/// task.
fn workers(page: &serde_json::Value, id: i64) -> Vec<(i64, Option<String>)> {
    if page["item"]["id"].as_i64() != Some(id) {
        return vec![(id, None)];
    }
    page["item"]["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["closed"] != true)
        .filter_map(|a| {
            let machine = a["machine"].as_str().unwrap_or(THIS_MACHINE);
            let remote = (machine != THIS_MACHINE).then(|| machine.to_string());
            Some((a["task"].as_i64()?, remote))
        })
        .collect()
}

/// The folder a task's agent works in: its live row's (a closed task's terminal number may be
/// another tab's, so the row is found by its task), else the one its record keeps: an asleep
/// tab's, or a claimed task's whose tab closed.
fn folder_of(rows: &[serde_json::Value], task: &Task) -> Option<PathBuf> {
    rows.iter()
        .find(|r| r["task"]["id"].as_i64() == Some(task.id))
        .and_then(|r| r["cwd"].as_str())
        .map(PathBuf::from)
        .or_else(|| task.folder.clone())
}

/// `--repo` for one task: qass's own word, before its folder. For an item's tasks it would
/// put every agent in one repo: it stands in only for a task with no folder.
fn flag_for<'a>(single: bool, cwd: Option<&Path>, flag: Option<&'a Path>) -> Option<&'a Path> {
    flag.filter(|_| single || cwd.is_none())
}

/// Reads every task the target covers: the daemon's record, its live row's folder, git,
/// gh and qass's approved copy.
fn gather(target: &str, flag: Option<&Path>) -> std::io::Result<Facts> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let page = json(&["task", "show", target, "--json"])
        .filter(|p| p["task"]["id"].is_i64())
        .ok_or_else(|| std::io::Error::other(format!("no task {target} could be read")))?;
    let id = page["task"]["id"].as_i64().unwrap_or_default();
    let workers = workers(&page, id);
    let single = page["item"]["id"].as_i64() != Some(id);
    let rows = json(&["agents", "--json"])
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let data = paths::qass_data_dir();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let works = workers
        .into_iter()
        .map(|(id, remote)| {
            if let Some(remote) = remote {
                return Ok(Work {
                    id,
                    remote: Some(remote),
                    ..Work::default()
                });
            }
            let task = task_of(id, now).ok_or(id)?;
            let cwd = folder_of(&rows, &task);
            Ok(work(
                &task,
                cwd.as_deref(),
                flag_for(single, cwd.as_deref(), flag),
                &path,
                data.as_deref(),
                home.as_deref(),
            ))
        })
        .collect();
    Ok(Facts {
        target: target.to_string(),
        works,
    })
}

/// `qoop task verify <KEY|id> [--repo <path>] [--json]`; `rest` is the words after `verify`.
pub(crate) fn run(rest: &[String]) -> std::io::Result<()> {
    let (mut target, mut flag, mut as_json) = (None, None, false);
    let mut it = rest.iter();
    while let Some(word) = it.next() {
        match word.as_str() {
            "--json" => as_json = true,
            "--repo" => flag = Some(PathBuf::from(it.next().ok_or_else(|| crate::usage(USAGE))?)),
            w if !w.starts_with('-') && target.is_none() => target = Some(w),
            _ => return Err(crate::usage(USAGE)),
        }
    }
    let target = target.ok_or_else(|| crate::usage(USAGE))?;
    let (lines, json) = compose(&gather(target, flag.as_deref())?);
    if as_json {
        println!("{json}");
    } else {
        println!("{}", lines.join("\n"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const OPENED: i64 = 1_791_000_000;

    fn iso(at: i64) -> String {
        format!(
            "{}T{}:{:02}Z",
            qoop_history::date_utc(at * 1000),
            clock(at, 0),
            at.rem_euclid(60)
        )
    }

    fn sh(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A scratch place: `demo` (a repo on main), a `bin` for the stub gh, a `data` for qass.
    struct Place {
        dir: tempfile::TempDir,
    }

    impl Place {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("dir");
            let main = dir.path().join("demo");
            std::fs::create_dir_all(&main).expect("main");
            sh(&main, &["init", "-q", "-b", "main"]);
            sh(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
            std::fs::create_dir_all(dir.path().join("bin")).expect("bin");
            std::fs::create_dir_all(dir.path().join("data")).expect("data");
            Self { dir }
        }

        fn at(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        /// A worktree of `branch` at `demo-wt-<folder>`, removed again when `gone`.
        fn worktree(&self, branch: &str, folder: &str, gone: bool) -> PathBuf {
            let path = self.at(&format!("demo-wt-{folder}"));
            let p = path.to_str().expect("utf8");
            sh(
                &self.at("demo"),
                &["worktree", "add", "-q", "-b", branch, p],
            );
            if gone {
                sh(&self.at("demo"), &["worktree", "remove", p]);
            }
            path
        }

        /// gh answering for `pat/demo` (default `main`) and these PRs: (number, base, head,
        /// created, merged).
        fn gh(&self, prs: &[(u64, &str, &str, i64, Option<i64>)]) {
            self.gh_open(prs, &[]);
        }

        /// The same, with open PRs (number, head) that `gh pr list --head` lists.
        fn gh_open(&self, prs: &[(u64, &str, &str, i64, Option<i64>)], open: &[(u64, &str)]) {
            let prs: Vec<_> = prs
                .iter()
                .map(|&(n, base, head, created, merged)| {
                    ("pat/demo", n, base, head, created, merged)
                })
                .collect();
            self.gh_repos(&prs, open);
        }

        /// gh answering for PRs of any repo: (repo, number, base, head, created, merged).
        #[allow(clippy::type_complexity)]
        fn gh_repos(
            &self,
            prs: &[(&str, u64, &str, &str, i64, Option<i64>)],
            open: &[(u64, &str)],
        ) {
            let mut heads = String::new();
            for (n, head) in open {
                heads += &format!("  {head}) echo '[{{\"number\":{n}}}]' ;;\n");
            }
            let mut cases = String::new();
            for (repo, n, base, head, created, merged) in prs {
                let pr = serde_json::json!({"baseRefName": base, "headRefName": head,
                    "createdAt": iso(*created), "mergedAt": merged.map(iso)});
                cases += &format!("  {repo}#{n}) echo '{pr}' ;;\n");
            }
            self.stub(&format!(
                "case \"$1 $2\" in\n\"repo view\") echo '{{\"nameWithOwner\":\"pat/demo\",\
                 \"defaultBranchRef\":{{\"name\":\"main\"}}}}' ;;\n\"pr view\") case \"$5#$3\" in\n\
                 {cases}  *) echo \"no pull request $3\" >&2; exit 1 ;;\n  esac ;;\n\
                 \"pr list\") while [ $# -gt 0 ]; do [ \"$1\" = --head ] && h=$2; shift; done\n\
                 case \"$h\" in\n{heads}  *) echo '[]' ;;\n  esac ;;\nesac\n"
            ));
        }

        fn stub(&self, body: &str) {
            let gh = self.at("bin/gh");
            std::fs::write(&gh, format!("#!/bin/sh\n{body}")).expect("gh");
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        fn path(&self) -> std::ffi::OsString {
            format!("{}:/usr/bin:/bin", self.at("bin").display()).into()
        }

        /// The approved copy naming an install at `bin/qoop`, its mtime `at`.
        fn installed(&self, at: i64) {
            let copy = format!(
                "# init\n\n## Verify where it runs\n\n- where: `installed {}`\n- check: `qoop --version`\n",
                self.at("bin/qoop").display()
            );
            std::fs::write(self.at("data/init-demo.md"), copy).expect("copy");
            let file = std::fs::File::create(self.at("bin/qoop")).expect("install");
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(at as u64);
            file.set_modified(when).expect("mtime");
        }

        fn verify(&self, evidence: &[&str], cwd: Option<&Path>, flag: Option<&Path>) -> String {
            let task = Task {
                id: 12,
                opened: OPENED,
                evidence: evidence.iter().map(|e| e.to_string()).collect(),
                ..Task::default()
            };
            let w = work(
                &task,
                cwd,
                flag,
                &self.path(),
                Some(&self.at("data")),
                Some(self.dir.path()),
            );
            let facts = Facts {
                target: "12".into(),
                works: vec![Ok(w)],
            };
            compose(&facts).0.join("\n")
        }
    }

    fn last(out: &str) -> &str {
        out.lines().last().unwrap_or("")
    }

    /// The worker's PR merged into main after the task opened, from its own worktree, which is
    /// gone, and the install is newer: nothing missing.
    #[test]
    fn a_good_pr_passes() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        p.installed(OPENED + 900);
        let out = p.verify(&["cargo test: ok; PR #45 merged"], Some(&cwd), None);
        assert_eq!(last(&out), PASSED, "{out}");
        assert!(out.contains("#12 PR #45 merged"), "{out}");
        assert!(out.contains("min newer than the merge"), "{out}");
    }

    /// A gone or asleep worker has no live row: `--repo` naming its `-wt-` folder, even gone,
    /// ties its PR; a main checkout given ties nothing.
    #[test]
    fn a_gone_worktree_given_as_repo_ties() {
        let p = Place::new();
        let gone = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        p.installed(OPENED + 900);
        let out = p.verify(&["PR #45"], None, Some(&gone));
        assert_eq!(last(&out), PASSED, "{out}");
        let out = p.verify(&["PR #45"], None, Some(&p.at("demo")));
        assert!(out.contains("PR #45 not tied to this task"), "{out}");
    }

    /// #45 merged, but #47 from the same branch is still open: the work is not all in.
    #[test]
    fn an_open_pr_from_the_same_branch_blocks() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh_open(
            &[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))],
            &[(47, "feat/x")],
        );
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(out.contains("#12 open PR #47 from feat/x too"), "{out}");
    }

    /// Closed tasks on an item were checked when they closed, before the item: a KEY counts
    /// only the open ones.
    #[test]
    fn a_key_counts_only_its_open_tasks() {
        let page = serde_json::json!({"task": {"id": 3}, "item": {"id": 3, "agents": [
            {"task": 12, "machine": "this Mac", "closed": true},
            {"task": 13, "machine": "this Mac", "closed": false},
            {"task": 14, "machine": "ubox"}]}});
        assert_eq!(workers(&page, 3), [(13, None), (14, Some("ubox".into()))]);
        let page = serde_json::json!({"task": {"id": 12}});
        assert_eq!(workers(&page, 12), [(12, None)]);
    }

    /// Only the `- where:` line says where it runs; a check naming an install does not.
    #[test]
    fn the_install_is_the_where_line_only() {
        let head = "# init\n\n## Verify where it runs\n\n";
        let text = format!("{head}- check: the installed /a copy\n- where: `installed /b`\n");
        assert_eq!(installed(&text).as_deref(), Some("/b"));
        let text = format!("{head}- where: local\n- check: run the installed /a copy\n");
        assert_eq!(installed(&text), None);
    }

    #[test]
    fn an_old_pr_or_one_created_before_the_task_blocks() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[
            (44, "main", "feat/x", OPENED - 900, Some(OPENED - 600)),
            (43, "main", "feat/x", OPENED - 900, Some(OPENED + 600)),
        ]);
        let out = p.verify(&["PR #44 is the base", "pull #43 merged"], Some(&cwd), None);
        assert!(
            out.contains("PR #44 merged before the task opened"),
            "{out}"
        );
        assert!(
            out.contains("PR #43 created before the task opened"),
            "{out}"
        );
    }

    /// Another repo's PR is asked only through its checkout beside the worker's repo.
    #[test]
    fn a_pr_of_another_repo_needs_its_checkout_beside() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(7, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        let out = p.verify(
            &["merged other/demo#7 and https://github.com/Other/Lib/pull/7/files"],
            Some(&cwd),
            None,
        );
        assert!(
            out.contains("other/demo#7: no checkout of other/demo beside"),
            "{out}"
        );
        assert!(
            out.contains("other/lib#7: no checkout of other/lib beside"),
            "{out}"
        );
        assert!(!out.contains("merged 20"), "{out}");
        // The same repo by URL or owner/repo is a PR like any other.
        let out = p.verify(&["https://github.com/pat/demo/pull/7"], Some(&cwd), None);
        assert!(out.contains("pat/demo#7 merged"), "{out}");
    }

    #[test]
    fn a_pr_into_another_base_blocks() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "dev", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(out.contains("PR #45 into dev, not main"), "{out}");
    }

    /// The worker sat in the main checkout: a merged same-repo PR is not tied to its task.
    #[test]
    fn a_pr_from_the_main_checkout_is_not_tied() {
        let p = Place::new();
        p.gh(&[(45, "main", "other-work", OPENED + 60, Some(OPENED + 600))]);
        let out = p.verify(&["PR #45"], Some(&p.at("demo")), None);
        assert!(out.contains("PR #45 not tied to this task"), "{out}");
        let cwd = p.worktree("feat/x", "feat-x", true);
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(
            out.contains("PR #45 from other-work, not this worker's worktree"),
            "{out}"
        );
    }

    #[test]
    fn evidence_with_no_pr_or_a_bare_number_blocks() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        for evidence in ["cargo test: ok", "#45 tests pass"] {
            let out = p.verify(&[evidence], Some(&cwd), None);
            assert!(out.contains("#12 no PR named in its evidence"), "{out}");
        }
        assert!(named_prs(["#45 tests", "step #3 done"]).is_empty());
        assert_eq!(
            named_prs(["PR #4, (pull #5) PR#6 pat/demo#7."])
                .iter()
                .map(Named::label)
                .collect::<Vec<_>>(),
            ["PR #4", "PR #5", "PR #6", "pat/demo#7"]
        );
        assert_eq!(
            named_prs(["http://www.github.com/pat/demo/pull/8 www.github.com/pat/demo/pull/9"])
                .iter()
                .map(Named::label)
                .collect::<Vec<_>>(),
            ["pat/demo#8", "pat/demo#9"]
        );
    }

    /// A gone folder maps back at the first `-wt-` whose prefix is a main checkout, so a
    /// slug holding `-wt-` still ties; with neither, `--repo` names the repo, never tied.
    #[test]
    fn a_gone_worktree_finds_its_repo_by_the_first_prefix() {
        let p = Place::new();
        // A decoy: the longer prefix is a repo too, and still the first one wins.
        let decoy = p.at("demo-wt-fix");
        std::fs::create_dir_all(&decoy).expect("decoy");
        sh(&decoy, &["init", "-q", "-b", "main"]);
        let cwd = p
            .worktree("fix/wt-thing", "fix-wt-thing", true)
            .join("crates");
        p.gh(&[(45, "main", "fix/wt-thing", OPENED + 60, Some(OPENED + 600))]);
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(out.contains("worktree fix-wt-thing"), "{out}");
        assert!(out.contains("#12 PR #45 merged"), "{out}");
        let elsewhere = p.at("gone/somewhere");
        let out = p.verify(&["PR #45"], Some(&elsewhere), None);
        assert!(
            out.contains("#12 folder ") && out.contains(" is gone"),
            "{out}"
        );
        let out = p.verify(&["PR #45"], Some(&elsewhere), Some(&p.at("demo")));
        assert!(out.contains("PR #45 not tied to this task"), "{out}");
    }

    /// A multi-repo task: its PR in the repo beside, found by its origin, passes from the same
    /// worktree slug; another slug or that repo's worktree still listed misses; the install
    /// is compared with this repo's merges only.
    #[test]
    fn a_pr_in_the_repo_beside_is_checked_there() {
        let p = Place::new();
        let lib = p.at("lib");
        std::fs::create_dir_all(&lib).expect("lib");
        sh(&lib, &["init", "-q", "-b", "main"]);
        sh(&lib, &["commit", "-q", "--allow-empty", "-m", "init"]);
        sh(
            &lib,
            &["remote", "add", "origin", "git@github.com:Pat/Lib.git"],
        );
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh_repos(
            &[
                (
                    "pat/demo",
                    45,
                    "main",
                    "feat/x",
                    OPENED + 60,
                    Some(OPENED + 600),
                ),
                (
                    "pat/lib",
                    7,
                    "main",
                    "feat/x",
                    OPENED + 60,
                    Some(OPENED + 1800),
                ),
                (
                    "pat/lib",
                    8,
                    "main",
                    "feat/y",
                    OPENED + 60,
                    Some(OPENED + 600),
                ),
            ],
            &[],
        );
        p.installed(OPENED + 900);
        let out = p.verify(&["PR #45; pat/lib#7 merged"], Some(&cwd), None);
        assert_eq!(last(&out), PASSED, "{out}");
        assert!(
            out.contains("pat/lib#7 merged") && out.contains("of pat/lib"),
            "{out}"
        );
        let out = p.verify(&["PR #45; pat/lib#8"], Some(&cwd), None);
        assert!(
            out.contains("pat/lib#8 from feat/y, not this worker's worktree"),
            "{out}"
        );
        let wt = lib.with_file_name("lib-wt-feat-x");
        sh(
            &lib,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/x",
                wt.to_str().expect("utf8"),
            ],
        );
        let out = p.verify(&["PR #45; pat/lib#7"], Some(&cwd), None);
        assert!(out.contains("worktree of feat/x still listed"), "{out}");
        // A second checkout of the same repo: not guessed between.
        let copy = p.at("lib-old");
        std::fs::create_dir_all(&copy).expect("copy");
        sh(&copy, &["init", "-q", "-b", "main"]);
        sh(
            &copy,
            &["remote", "add", "origin", "https://github.com/pat/lib"],
        );
        let out = p.verify(&["PR #45; pat/lib#7"], Some(&cwd), None);
        assert!(
            out.contains("pat/lib#7: 2 checkouts of pat/lib beside"),
            "{out}"
        );
        assert!(github_name("https://github.com/o/r").as_deref() == Some("o/r"));
        assert!(github_name("ssh://git@github.com/O/R.git").as_deref() == Some("o/r"));
        assert_eq!(github_name("git@gitlab.com:o/r.git"), None);
    }

    /// No live row: the folder its record keeps finds the repo; `--repo` comes before both.
    #[test]
    fn the_folder_comes_from_repo_then_the_row_then_the_record() {
        let row = serde_json::json!([{"task": {"id": 12}, "cwd": "/live"}]);
        let rows = row.as_array().expect("rows");
        let task = Task {
            id: 12,
            folder: Some("/kept".into()),
            ..Task::default()
        };
        assert_eq!(folder_of(rows, &task), Some(PathBuf::from("/live")));
        assert_eq!(folder_of(&[], &task), Some(PathBuf::from("/kept")));
        let p = Place::new();
        let gone = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        p.installed(OPENED + 900);
        // Its tab sat in the main checkout; qass names its worktree.
        let out = p.verify(&["PR #45"], Some(&p.at("demo")), Some(&gone));
        assert_eq!(last(&out), PASSED, "{out}");
        let out = p.verify(&["PR #45"], None, None);
        assert!(
            out.contains("#12 no folder recorded; pass --repo <its -wt- folder>"),
            "{out}"
        );
    }

    /// Work in a folder in no repo passes on its evidence alone; a PR it names, no evidence,
    /// or a folder agents only start from (the home folder, one holding repos) does not.
    #[test]
    fn non_git_work_passes_on_its_evidence() {
        let p = Place::new();
        let notes = p.at("notes");
        std::fs::create_dir_all(&notes).expect("notes");
        let out = p.verify(
            &["report written: notes/scan.md, 40 rows"],
            Some(&notes),
            None,
        );
        assert_eq!(last(&out), PASSED, "{out}");
        assert!(
            out.contains("is not a git folder: evidence only, read it"),
            "{out}"
        );
        let out = p.verify(&["merged PR #9"], Some(&notes), None);
        assert!(
            out.contains("#12 names PR #9 but its folder is no repo"),
            "{out}"
        );
        let out = p.verify(&[], Some(&notes), None);
        assert!(out.contains("#12 no evidence"), "{out}");
        let out = p.verify(&["done"], Some(p.dir.path()), None);
        assert!(out.contains("is no repo; pass --repo"), "{out}");
        assert_eq!(
            place_of(&notes, Some(&notes)),
            Folder::NoRepo(notes.clone())
        );
        assert_eq!(place_of(&notes, None), Folder::NoRepo(notes.clone()));
        assert_eq!(place_of(Path::new("/"), None), Folder::NoRepo("/".into()));
        let hidden = p.at(".cache/x");
        std::fs::create_dir_all(&hidden).expect("hidden");
        assert_eq!(
            place_of(&hidden, Some(p.dir.path())),
            Folder::NoRepo(hidden)
        );
        // Inside a repo git could not read: not non-git work.
        let inside = p.at("demo/docs");
        std::fs::create_dir_all(&inside).expect("docs");
        assert_eq!(
            place_of(&inside, Some(p.dir.path())),
            Folder::NoRepo(inside)
        );
        // An item's tasks keep their own folders; `--repo` fills in only where one is missing.
        let (a, b) = (Path::new("/a"), Path::new("/b"));
        assert_eq!(flag_for(true, Some(a), Some(b)), Some(b));
        assert_eq!(flag_for(false, Some(a), Some(b)), None);
        assert_eq!(flag_for(false, None, Some(b)), Some(b));
    }

    #[test]
    fn gh_failing_or_missing_is_never_a_pass() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.stub("echo 'HTTP 401: Bad credentials' >&2\nexit 1\n");
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(
            last(&out).contains("#12 gh failed: HTTP 401: Bad credentials"),
            "{out}"
        );
        std::fs::remove_file(p.at("bin/gh")).expect("rm");
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(last(&out).contains("#12 gh not found"), "{out}");
    }

    #[test]
    fn a_worktree_still_listed_blocks() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", false);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(out.contains("#12 worktree of feat/x still listed"), "{out}");
    }

    #[test]
    fn an_install_older_than_the_merge_blocks() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        p.installed(OPENED + 300);
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(out.contains("older than the merge"), "{out}");
        assert!(
            out.contains(&p.at("bin/qoop").display().to_string()),
            "{out}"
        );
    }

    /// The repo's own workflow file is the worker's text: an install path it names is never
    /// looked at.
    #[test]
    fn a_repo_workflow_naming_another_install_is_ignored() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        p.gh(&[(45, "main", "feat/x", OPENED + 60, Some(OPENED + 600))]);
        let old = p.at("old-qoop");
        std::fs::write(&old, "").expect("old");
        let file = std::fs::File::options()
            .write(true)
            .open(&old)
            .expect("open");
        file.set_modified(std::time::UNIX_EPOCH).expect("mtime");
        std::fs::create_dir_all(p.at("demo/.qoop")).expect("qoop");
        std::fs::write(
            p.at("demo/.qoop/workflow.md"),
            format!(
                "## Verify where it runs\n\n- where: installed {}\n",
                old.display()
            ),
        )
        .expect("workflow");
        let out = p.verify(&["PR #45"], Some(&cwd), None);
        assert!(
            out.contains("install: not checked (no approved copy init-demo.md)"),
            "{out}"
        );
        assert_eq!(last(&out), PASSED, "{out}");
    }

    /// Two tasks on one item, one bad: only the bad one is missing, by its id.
    #[test]
    fn a_key_with_two_linked_tasks_one_bad() {
        let p = Place::new();
        let good = p.worktree("feat/x", "feat-x", true);
        let bad = p.worktree("feat/y", "feat-y", true);
        p.gh(&[
            (45, "main", "feat/x", OPENED + 60, Some(OPENED + 600)),
            (46, "main", "feat/y", OPENED + 60, None),
        ]);
        let task = |id, evidence: &str| Task {
            id,
            opened: OPENED,
            evidence: vec![evidence.to_string()],
            ..Task::default()
        };
        let data = p.at("data");
        let works = vec![
            Ok(work(
                &task(12, "PR #45"),
                Some(&good),
                None,
                &p.path(),
                Some(&data),
                None,
            )),
            Ok(work(
                &task(13, "PR #46"),
                Some(&bad),
                None,
                &p.path(),
                Some(&data),
                None,
            )),
            Ok(Work {
                id: 14,
                remote: Some("ubox".into()),
                ..Work::default()
            }),
        ];
        let (lines, json) = compose(&Facts {
            target: "QOOP-3".into(),
            works,
        });
        assert_eq!(
            lines.last().map(String::as_str),
            Some("Missing: #13 PR #46 not merged; #14 remote ubox: ask the owner"),
            "{lines:?}"
        );
        assert_eq!(json["passed"], false);
        assert_eq!(json["tasks"], serde_json::json!([12, 13, 14]));
    }

    #[test]
    fn a_verify_is_at_most_200_words() {
        let p = Place::new();
        let cwd = p.worktree("feat/x", "feat-x", true);
        let prs: Vec<_> = (1..=60)
            .map(|n| (n, "main", "feat/x", OPENED - 900, Some(OPENED - 600)))
            .collect();
        p.gh(&prs);
        let evidence: Vec<String> = (1..=60).map(|n| format!("PR #{n} merged")).collect();
        let evidence: Vec<&str> = evidence.iter().map(String::as_str).collect();
        let out = p.verify(&evidence, Some(&cwd), None);
        assert!(out.split_whitespace().count() <= WORDS, "{out}");
        assert!(
            last(&out).starts_with("Missing: #12 PR #1 merged before"),
            "{out}"
        );
        assert!(last(&out).ends_with(" more"), "{out}");
        // Many linked tasks: the lines before the last are cut with `+N more`.
        let works = (1..=40)
            .map(|id| {
                Ok(Work {
                    id,
                    evidence: vec!["cargo test: ok".into()],
                    ..Work::default()
                })
            })
            .collect();
        let (lines, _) = compose(&Facts {
            target: "QOOP-3".into(),
            works,
        });
        let out = lines.join("\n");
        assert!(out.split_whitespace().count() <= WORDS, "{out}");
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with(" more") && l.starts_with('+')),
            "{out}"
        );
    }
}

//! `qoop plugin install|link|ls|rm|run`: herdr plugins on this machine, and the consent each
//! one runs under.
//!
//! A plugin is a program the person chose, with build steps that run arbitrary code, so the
//! boundary is consent bound to content (ADR-018's amendment). Install shows everything the
//! manifest will run and the commit it came from, and records the person's yes for exactly
//! that manifest: `<data>/plugins/consent.json`, ADR-015's ledger pattern, keeping the vetted
//! manifest text itself. Every later run compares the manifest on disk with it and refuses
//! when it moved or the ledger is gone (fail closed). The commit is recorded, not checked
//! again: what consent covers is the manifest.
//!
//! Git only ever runs with an argv, never a shell, with repository hooks and `ext::` remotes
//! off, submodules left alone and none of the caller's git environment. The tree that was
//! shown is the tree moved into place; it is never fetched a second time after the preview.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use qoop_runtime::paths;
use serde::{Deserialize, Serialize};

use super::manifest::{self, Manifest};

const USAGE: &str = "usage:\n\
  qoop plugin install <owner/repo[/subdir]> [--ref REF] [--yes]  fetch a herdr plugin from\n\
  \x20                 GitHub, show what it runs, build it once you agree\n\
  qoop plugin link <dir> [--yes]  use a plugin from a folder as it is\n\
  qoop plugin ls [--json]  the plugins here and whether each still matches its consent\n\
  qoop plugin rm <id>  remove one (a linked folder is left as it is)\n\
  qoop plugin run <id.action>  run one of a plugin's actions";

/// What every plugin is told it can do, shown before any yes.
const POWERS: &str = "It runs as you, can read every terminal and the command lines of their \
                      processes, can type into terminals and run commands in shells.";

fn fail(msg: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    let rest: Vec<&str> = args.iter().skip(3).map(String::as_str).collect();
    let flag = |name: &str| rest.contains(&name);
    let plain: Vec<&str> = {
        let mut out = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            match rest[i] {
                "--ref" => i += 1,
                a if a.starts_with("--") || a == "-y" => {}
                a => out.push(a),
            }
            i += 1;
        }
        out
    };
    let home = Home::here()?;
    let yes = flag("--yes") || flag("-y");
    match (args.get(2).map(String::as_str), plain.as_slice()) {
        (Some("install"), [source]) => {
            let git_ref = match rest.iter().position(|a| *a == "--ref") {
                Some(i) => Some(*rest.get(i + 1).ok_or_else(|| fail("--ref needs a value"))?),
                None => None,
            };
            let source = Source::parse(source).map_err(fail)?;
            let subdir = source.subdir.as_deref();
            install_from(&home, &source.url, subdir, &source.shown, git_ref, yes)
        }
        (Some("link"), [dir]) => link(&home, Path::new(dir), yes),
        (Some("ls"), []) => ls(&home, flag("--json")),
        (Some("rm"), [id]) => rm(&home, id),
        (Some("run"), [spec]) => run_action(&home, spec),
        (Some("--help" | "-h") | None, _) => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(fail(USAGE)),
    }
}

/// Where plugins live: `<data>/plugins`, one folder (or a link, for `link`) per id, beside the
/// consent ledger and its lock. Dot names are qoop's own (`.bin`, `.state`, install scratch,
/// the ledger's temporaries), never an id.
pub(crate) struct Home {
    root: PathBuf,
    /// `<config>/plugins`: each plugin's config folder, `HERDR_PLUGIN_CONFIG_DIR`.
    config: PathBuf,
}

impl Home {
    fn here() -> std::io::Result<Self> {
        let data = paths::data_dir().ok_or_else(|| fail("no HOME to keep plugins under"))?;
        Ok(Self {
            root: data.join("plugins"),
            config: paths::config_dir().join("plugins"),
        })
    }

    fn ledger(&self) -> PathBuf {
        self.root.join("consent.json")
    }

    fn dir_of(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn state_of(&self, id: &str) -> PathBuf {
        self.root.join(".state").join(id)
    }

    fn config_of(&self, id: &str) -> PathBuf {
        self.config.join(id)
    }

    /// The folder the herdr shim is on `PATH` from.
    fn bin(&self) -> PathBuf {
        self.root.join(".bin")
    }
}

/// What the person agreed to for one plugin: the manifest text itself, the commit it came
/// from (recorded, never a pin; none for a link), and where it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Consent {
    pub(crate) sha: Option<String>,
    pub(crate) manifest: String,
    pub(crate) source: String,
}

/// The consent ledger. Opened the ADR-015 way: anything unreadable is empty, so a deleted or
/// broken ledger trusts nothing and every plugin asks again. Changed under a lock, written
/// whole to a fresh temporary and renamed, so two installs at once both land and a cut write
/// leaves the previous file.
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    plugins: BTreeMap<String, Consent>,
}

impl Ledger {
    fn open(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The ledger about to be written. One that does not parse is kept aside as
    /// `consent.json.bad-<unix time>` rather than written over, so what it held can still be
    /// read by a person; the new one starts empty, which trusts nothing.
    fn open_for_writing(path: &Path) -> std::io::Result<Self> {
        let Ok(bytes) = std::fs::read(path) else {
            return Ok(Self::default());
        };
        if let Ok(ledger) = serde_json::from_slice(&bytes) {
            return Ok(ledger);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let aside = path.with_file_name(format!("consent.json.bad-{now}"));
        std::fs::rename(path, &aside)?;
        eprintln!(
            "consent.json did not parse; kept it as {} and started a new one, so every other \
             plugin asks for consent again",
            aside.display()
        );
        Ok(Self::default())
    }

    fn update(home: &Home, change: impl FnOnce(&mut Self)) -> std::io::Result<()> {
        std::fs::create_dir_all(&home.root)?;
        let lock = std::fs::File::create(home.root.join("consent.lock"))?;
        // SAFETY: flock on a descriptor this function owns; released when `lock` closes.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut ledger = Self::open_for_writing(&home.ledger())?;
        change(&mut ledger);
        let mut temp = tempfile::Builder::new()
            .prefix(".consent-")
            .tempfile_in(&home.root)?;
        temp.write_all(&serde_json::to_vec_pretty(&ledger).map_err(fail)?)?;
        temp.persist(home.ledger()).map_err(|e| e.error)?;
        Ok(())
    }
}

/// A manifest's text, read with a bound: a bigger file is refused, not cut.
fn read_manifest(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(manifest::MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > manifest::MAX_BYTES {
        return Err(fail(format!(
            "{} is over {} bytes",
            manifest::FILE,
            manifest::MAX_BYTES
        )));
    }
    String::from_utf8(bytes).map_err(|_| fail(format!("{} is not UTF-8", manifest::FILE)))
}

/// `owner/repo[/subdir]`, the only form install takes from a person: always GitHub over https.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Source {
    url: String,
    subdir: Option<PathBuf>,
    shown: String,
}

impl Source {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let refuse = || format!("{text:?} is not owner/repo[/subdir] (GitHub only)");
        let parts: Vec<&str> = text.split('/').collect();
        let name_ok = |p: &str| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        let [owner, repo, subdir @ ..] = parts.as_slice() else {
            return Err(refuse());
        };
        if !name_ok(owner) || owner.starts_with('-') || !name_ok(repo) {
            return Err(refuse());
        }
        if subdir
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            return Err(refuse());
        }
        Ok(Self {
            url: format!("https://github.com/{owner}/{repo}"),
            subdir: (!subdir.is_empty()).then(|| subdir.iter().collect()),
            shown: text.to_string(),
        })
    }
}

/// Whether a variable would point git somewhere other than the clone it is asked about, or
/// add configuration the clone did not choose.
fn gits(name: &OsStr) -> bool {
    let name = name.as_encoded_bytes();
    name.starts_with(b"GIT_CONFIG")
        || [
            b"GIT_DIR".as_slice(),
            b"GIT_WORK_TREE",
            b"GIT_INDEX_FILE",
            b"GIT_OBJECT_DIRECTORY",
            b"GIT_ALTERNATE_OBJECT_DIRECTORIES",
            b"GIT_COMMON_DIR",
            b"GIT_NAMESPACE",
            b"GIT_SHALLOW_FILE",
        ]
        .contains(&name)
}

/// Git with argv only, hooks and `ext::` remotes off, none of the caller's git environment,
/// never asking for a password.
fn git(dir: Option<&Path>, args: &[&OsStr]) -> std::io::Result<String> {
    let mut cmd = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if gits(&name) {
            cmd.env_remove(name);
        }
    }
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "protocol.ext.allow=never",
    ]);
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    let out = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(fail(format!(
            "git {}: {}",
            args.first()
                .map_or(String::new(), |a| a.to_string_lossy().into_owned()),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Clone `url` into `into` and check out `git_ref` (or the default branch), returning the
/// commit. The ref is never an option: one starting with `-` is refused, and it is passed after
/// `--end-of-options`.
fn fetch(url: &str, git_ref: Option<&str>, into: &Path) -> std::io::Result<String> {
    if git_ref.is_some_and(|r| r.starts_with('-') || r.is_empty()) {
        return Err(fail("--ref cannot be empty or start with `-`"));
    }
    git(
        None,
        &[
            OsStr::new("clone"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new("--quiet"),
            OsStr::new("--"),
            OsStr::new(url),
            into.as_os_str(),
        ],
    )?;
    let Some(wanted) = git_ref else {
        return git(Some(into), &["rev-parse", "HEAD"].map(OsStr::new));
    };
    // A branch is only `origin/<name>` in a fresh clone; a tag or a commit is itself.
    let commit = [
        format!("{wanted}^{{commit}}"),
        format!("origin/{wanted}^{{commit}}"),
    ]
    .iter()
    .find_map(|spec| {
        let args = ["rev-parse", "--verify", "--quiet", "--end-of-options", spec];
        git(Some(into), &args.map(OsStr::new)).ok()
    })
    .ok_or_else(|| fail(format!("{url} has no ref {wanted:?}")))?;
    git(
        Some(into),
        &["checkout", "--quiet", "--detach", commit.as_str()].map(OsStr::new),
    )?;
    Ok(commit)
}

/// The plugin root inside a clone, resolved: a `subdir` that leads out of the clone (through
/// `..` or a symlink) is refused, and so is a manifest that is itself a link.
fn root_in(clone: &Path, subdir: Option<&Path>) -> std::io::Result<PathBuf> {
    let inside = clone.canonicalize()?;
    let root = match subdir {
        Some(sub) => clone
            .join(sub)
            .canonicalize()
            .map_err(|_| fail(format!("{} is not in the repository", sub.display())))?,
        None => inside.clone(),
    };
    let manifest = root
        .join(manifest::FILE)
        .canonicalize()
        .map_err(|_| fail(format!("no {} at the plugin's root", manifest::FILE)))?;
    if !root.starts_with(&inside) || manifest != root.join(manifest::FILE) || !manifest.is_file() {
        return Err(fail(
            "the plugin's folder or manifest leads out of the repository",
        ));
    }
    Ok(root)
}

/// What install and link show before any yes: every command verbatim (each argument quoted,
/// with control and direction characters escaped, so a manifest cannot hide one) and the
/// commit it came from.
fn preview(m: &Manifest, from: &str, sha: Option<&str>) -> String {
    let here = m.here();
    let shown = |s: &str| s.escape_debug().to_string();
    let argv = |c: &[String]| format!("{c:?}");
    let mut out = format!(
        "plugin   {} {}\n",
        m.id,
        shown(m.version.as_deref().unwrap_or("(no version)"))
    );
    out += &format!("from     {}\n", shown(from));
    out += &match sha {
        Some(sha) => format!("commit   {sha} (recorded; your yes covers the manifest below)\n"),
        None => "commit   none: a linked folder runs whatever it holds; your yes covers the \
                 manifest below\n"
            .to_string(),
    };
    for step in &here.build {
        out += &format!("build    {}\n", argv(&step.command));
    }
    for a in &here.actions {
        out += &format!("action   {} {}\n", shown(&a.id), argv(&a.command));
    }
    for p in &here.panes {
        let placement = p.placement.as_deref().unwrap_or("overlay");
        out += &format!(
            "pane     {} ({}) {}\n",
            shown(&p.id),
            shown(placement),
            argv(&p.command)
        );
    }
    for e in &here.events {
        out += &format!("event    {} {}\n", shown(&e.on), argv(&e.command));
    }
    for s in &here.startup {
        out += &format!("startup  {}\n", argv(&s.command));
    }
    if m.wants_newer_herdr() {
        out += &format!(
            "warning  it asks for herdr {}; qoop answers as herdr {}, so some of it may not work\n",
            shown(m.min_herdr_version.as_deref().unwrap_or_default()),
            manifest::HERDR_VERSION
        );
    }
    out + POWERS + "\n"
}

/// The person's yes, asked only on a terminal; anywhere else they say it with `--yes`.
fn agreed(yes: bool, what: &str) -> std::io::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(fail(format!(
            "not a terminal: say yes with `qoop plugin {what} ... --yes`"
        )));
    }
    print!("{what} it? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// What refuses a plugin before anything is shown or asked: another platform, or an id that is
/// already here (replacing one is `rm` first, so nothing is replaced behind a preview).
fn placeable(home: &Home, m: &Manifest) -> std::io::Result<()> {
    if !m.runs_here() {
        return Err(fail(format!(
            "{} does not run on {}: it names {:?}",
            m.id,
            std::env::consts::OS,
            m.platforms.as_deref().unwrap_or_default()
        )));
    }
    if std::fs::symlink_metadata(home.dir_of(&m.id)).is_ok() {
        return Err(fail(format!(
            "{} is already installed; `qoop plugin rm {}` first",
            m.id, m.id
        )));
    }
    Ok(())
}

/// Say so when a config folder of an earlier plugin with this id is still there: the new one
/// is handed it.
fn note_kept_config(home: &Home, id: &str) {
    let config = home.config_of(id);
    if config.is_dir() {
        println!(
            "note     {id} keeps the config folder an earlier install left: {}",
            config.display()
        );
    }
}

/// Install from any git url. Only tests hand it something other than a GitHub https url; the
/// command builds one from `owner/repo` and nothing else.
fn install_from(
    home: &Home,
    url: &str,
    subdir: Option<&Path>,
    shown: &str,
    git_ref: Option<&str>,
    yes: bool,
) -> std::io::Result<()> {
    std::fs::create_dir_all(&home.root)?;
    let scratch = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&home.root)?;
    let clone = scratch.path().join("clone");
    eprintln!("fetching {url}");
    let sha = fetch(url, git_ref, &clone)?;
    let root = root_in(&clone, subdir)?;
    let text = read_manifest(&root.join(manifest::FILE))?;
    let m = Manifest::parse(&text).map_err(fail)?;
    placeable(home, &m)?;
    print!("{}", preview(&m, shown, Some(&sha)));
    note_kept_config(home, &m.id);
    if !agreed(yes, "install")? {
        println!("nothing was installed");
        return Ok(());
    }
    let dest = home.dir_of(&m.id);
    std::fs::rename(&root, &dest)?;
    build(home, &m, &dest).map_err(|e| {
        fail(format!(
            "{}'s build failed, so nothing was recorded and it will not run: {e}\n\
             `qoop plugin rm {}` clears it",
            m.id, m.id
        ))
    })?;
    Ledger::update(home, |ledger| {
        ledger.plugins.insert(
            m.id.clone(),
            Consent {
                sha: Some(sha.clone()),
                manifest: text,
                source: shown.to_string(),
            },
        );
    })?;
    println!(
        "installed {}, commit recorded {}",
        m.id,
        &sha[..sha.len().min(12)]
    );
    Ok(())
}

/// Run the manifest's build steps for this platform, in the plugin's root, with the plugin's
/// environment and none of qoop's own.
fn build(home: &Home, m: &Manifest, root: &Path) -> std::io::Result<()> {
    for step in &m.here().build {
        eprintln!("building: {:?}", step.command);
        let mut cmd = Command::new(&step.command[0]);
        cmd.args(&step.command[1..]).current_dir(root);
        cmd.envs(plugin_env(home, &m.id, root));
        scrub(&mut cmd);
        let status = cmd.status()?;
        if !status.success() {
            return Err(fail(format!("{:?} failed ({status})", step.command)));
        }
    }
    Ok(())
}

/// Whether a variable is qoop's own and never reaches a plugin: a terminal's token, the
/// runtime it talks to, the old names of both.
fn qoops(name: &OsStr) -> bool {
    let name = name.as_encoded_bytes();
    name.starts_with(b"QOOP_") || name.starts_with(b"QORO_")
}

/// Strip qoop's variables and a parent coding agent's from what `cmd` would hand on, whether
/// inherited or set on it.
pub(crate) fn scrub(cmd: &mut Command) {
    let set: Vec<OsString> = cmd.get_envs().map(|(name, _)| name.to_owned()).collect();
    let inherited = std::env::vars_os().map(|(name, _)| name);
    for name in set.into_iter().chain(inherited) {
        if qoops(&name) {
            cmd.env_remove(name);
        }
    }
    qoop_runtime::daemon::scrub_parent_agent_env(cmd);
}

/// The environment every command of a plugin runs with, as herdr sets it. The event, context
/// and pane variables are the host's to add.
pub(crate) fn plugin_env(home: &Home, id: &str, root: &Path) -> Vec<(String, OsString)> {
    let state = home.state_of(id);
    let config = home.config_of(id);
    let _ = std::fs::create_dir_all(&state);
    let _ = std::fs::create_dir_all(&config);
    let mut path = OsString::from(home.bin());
    if let Some(inherited) = std::env::var_os("PATH") {
        path.push(":");
        path.push(inherited);
    }
    // Where the host listens; read only, nothing is created for it here.
    let socket = paths::session_dir(&paths::base_location(), "default").join("herdr.sock");
    vec![
        ("HERDR_ENV".into(), "1".into()),
        ("HERDR_SESSION".into(), "default".into()),
        ("HERDR_BIN_PATH".into(), home.bin().join("herdr").into()),
        ("HERDR_PLUGIN_ID".into(), id.into()),
        ("HERDR_PLUGIN_ROOT".into(), root.into()),
        ("HERDR_PLUGIN_CONFIG_DIR".into(), config.into()),
        ("HERDR_PLUGIN_STATE_DIR".into(), state.into()),
        ("HERDR_SOCKET_PATH".into(), socket.clone().into()),
        ("HERDR_CLIENT_SOCKET_PATH".into(), socket.into()),
        ("PATH".into(), path),
    ]
}

fn link(home: &Home, dir: &Path, yes: bool) -> std::io::Result<()> {
    let dir = dir
        .canonicalize()
        .map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    if home
        .root
        .canonicalize()
        .is_ok_and(|inside| dir.starts_with(inside))
    {
        return Err(fail(format!(
            "{} is inside qoop's own plugin folder; link a folder of yours",
            dir.display()
        )));
    }
    let text = read_manifest(&dir.join(manifest::FILE))
        .map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    let m = Manifest::parse(&text).map_err(fail)?;
    placeable(home, &m)?;
    let from = format!("link {}", dir.display());
    print!("{}", preview(&m, &from, None));
    note_kept_config(home, &m.id);
    if !agreed(yes, "link")? {
        println!("nothing was linked");
        return Ok(());
    }
    std::fs::create_dir_all(&home.root)?;
    std::os::unix::fs::symlink(&dir, home.dir_of(&m.id))?;
    Ledger::update(home, |ledger| {
        ledger.plugins.insert(
            m.id.clone(),
            Consent {
                sha: None,
                manifest: text,
                source: from,
            },
        );
    })?;
    println!(
        "linked {} to {}; no build steps ran, and no commit is recorded",
        m.id,
        dir.display()
    );
    Ok(())
}

/// A plugin that may run now: its manifest is still the text the person agreed to.
pub(crate) struct Admitted {
    pub(crate) manifest: Manifest,
    pub(crate) root: PathBuf,
    pub(crate) consent: Consent,
}

/// Check `id` against the ledger, every time before anything of it runs. A missing ledger, a
/// missing entry or a manifest that changed refuses.
pub(crate) fn admitted(home: &Home, id: &str) -> Result<Admitted, String> {
    if !manifest::valid_id(id) {
        return Err(format!("{id:?} is not a plugin id"));
    }
    let root = home
        .dir_of(id)
        .canonicalize()
        .map_err(|_| format!("{id} is not installed"))?;
    let text = read_manifest(&root.join(manifest::FILE)).map_err(|e| format!("{id}: {e}"))?;
    let consent = Ledger::open(&home.ledger())
        .plugins
        .remove(id)
        .ok_or_else(|| format!("{id} has no consent recorded; install or link it again"))?;
    if consent.manifest.as_bytes() != text.as_bytes() {
        return Err(format!(
            "{id}'s manifest changed since you agreed to it; `qoop plugin rm {id}` and install \
             or link it again to see what it runs now"
        ));
    }
    let manifest = Manifest::parse(&text)?;
    if manifest.id != id {
        return Err(format!("{id}'s folder holds plugin {}", manifest.id));
    }
    Ok(Admitted {
        manifest,
        root,
        consent,
    })
}

/// Every plugin id here, in name order, a link whose folder is gone among them.
fn installed(home: &Home) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(&home.root) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| manifest::valid_id(name))
        .collect();
    ids.sort();
    ids
}

fn ls(home: &Home, json: bool) -> std::io::Result<()> {
    let rows: Vec<serde_json::Value> = installed(home)
        .iter()
        .map(|id| {
            let path = home.dir_of(id);
            let linked = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            let broken = linked && path.canonicalize().is_err();
            let checked = if broken {
                Err(format!(
                    "broken link to {}",
                    std::fs::read_link(&path).unwrap_or_default().display()
                ))
            } else {
                admitted(home, id)
            };
            let (version, sha, source) = match &checked {
                Ok(a) => (
                    a.manifest.version.clone(),
                    a.consent.sha.clone(),
                    Some(a.consent.source.clone()),
                ),
                Err(_) => (None, None, None),
            };
            serde_json::json!({
                "id": id,
                "version": version,
                "linked": linked,
                "broken": broken,
                "commit_recorded": sha,
                "source": source,
                "root": checked.as_ref().map_or_else(|_| path.canonicalize().ok(), |a| Some(a.root.clone())),
                "ok": checked.is_ok(),
                "refused": checked.err(),
            })
        })
        .collect();
    if json {
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if rows.is_empty() {
        println!("no plugins; add one with `qoop plugin install owner/repo`");
        return Ok(());
    }
    println!(
        "{:<28} {:<10} {:<16} STATE",
        "ID", "VERSION", "COMMIT RECORDED"
    );
    for row in &rows {
        let commit = match (row["linked"].as_bool(), row["commit_recorded"].as_str()) {
            (Some(true), _) => "none (linked)".to_string(),
            (_, Some(sha)) => sha[..sha.len().min(12)].to_string(),
            _ => "-".to_string(),
        };
        println!(
            "{:<28} {:<10} {:<16} {}",
            row["id"].as_str().unwrap_or_default(),
            row["version"].as_str().unwrap_or("-"),
            commit,
            row["refused"].as_str().unwrap_or("ok")
        );
    }
    Ok(())
}

/// Take away a plugin folder: a link is unlinked and its target left alone; a folder is
/// removed only when it really is directly under the plugins home.
fn remove_entry(home: &Home, path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return std::fs::remove_file(path);
    }
    let inside = home.root.canonicalize()?;
    let real = path.canonicalize()?;
    if real.parent() != Some(inside.as_path()) || !meta.is_dir() {
        return Err(fail(format!("{} is not a plugin folder", path.display())));
    }
    std::fs::remove_dir_all(real)
}

fn rm(home: &Home, id: &str) -> std::io::Result<()> {
    if !manifest::valid_id(id) {
        return Err(fail(format!("{id:?} is not a plugin id")));
    }
    let path = home.dir_of(id);
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(fail(format!("{id} is not installed")));
    }
    // Consent goes first: whatever fails after this, nothing of it runs again.
    Ledger::update(home, |ledger| {
        ledger.plugins.remove(id);
    })?;
    remove_entry(home, &path)?;
    // Not followed if a link: remove_dir_all takes a link itself away.
    match std::fs::remove_dir_all(home.state_of(id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    println!("removed {id}");
    let config = home.config_of(id);
    if config.is_dir() {
        println!("its config stays at {}", config.display());
    }
    Ok(())
}

/// `<id>.<action>`. herdr's action ids hold no dots, so the action is what follows the last
/// one and the id is everything before it.
fn resolve(home: &Home, spec: &str) -> Option<(String, String)> {
    let (id, action) = spec.rsplit_once('.')?;
    (!action.is_empty() && installed(home).iter().any(|i| i == id))
        .then(|| (id.to_string(), action.to_string()))
}

fn run_action(home: &Home, spec: &str) -> std::io::Result<()> {
    let (id, action) =
        resolve(home, spec).ok_or_else(|| fail(format!("no plugin action {spec}")))?;
    let plugin = admitted(home, &id).map_err(fail)?;
    let found = plugin
        .manifest
        .here()
        .actions
        .into_iter()
        .find(|a| a.id == action)
        .ok_or_else(|| fail(format!("{id} has no action {action} on this machine")))?;
    // ponytail: checked and resolved only; running it needs the host's socket and context.
    Err(fail(format!(
        "{id}.{} is installed and agreed to, but running plugin actions is not available yet",
        found.id
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, Home) {
        let dir = tempfile::tempdir().expect("dir");
        let home = Home {
            root: dir.path().join("data/plugins"),
            config: dir.path().join("config/plugins"),
        };
        (dir, home)
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git")
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repository holding `manifest` in `sub/`, committed.
    fn repo_with(at: &Path, manifest: &str) -> PathBuf {
        let repo = at.join("upstream");
        std::fs::create_dir_all(repo.join("sub")).expect("mkdir");
        std::fs::write(repo.join("sub").join(manifest::FILE), manifest).expect("manifest");
        git_in(at, &["init", "--quiet", "upstream"]);
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "--quiet", "-m", "plugin"]);
        repo
    }

    /// A plugin whose build writes what it was given.
    fn repo(at: &Path, id: &str) -> PathBuf {
        repo_with(
            at,
            &format!(
                "id = \"{id}\"\nversion = \"0.1.0\"\n\
                 [[build]]\ncommand = [\"sh\", \"-c\", \"env > built.txt\"]\n\
                 [[actions]]\nid = \"hello\"\ncommand = [\"echo\", \"hi\"]\n"
            ),
        )
    }

    fn folder(at: &Path, name: &str, manifest: &str) -> PathBuf {
        let dir = at.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(manifest::FILE), manifest).expect("manifest");
        dir
    }

    fn install(home: &Home, url: &Path, git_ref: Option<&str>) -> std::io::Result<()> {
        let url = url.to_str().expect("utf-8");
        install_from(home, url, Some(Path::new("sub")), "t/p/sub", git_ref, true)
    }

    #[test]
    fn only_owner_repo_shorthand_is_taken() {
        let s = Source::parse("persiyanov/herdr-reviewr").expect("ok");
        assert_eq!(s.url, "https://github.com/persiyanov/herdr-reviewr");
        assert_eq!(s.subdir, None);
        let s = Source::parse("a/b/plugins/x").expect("ok");
        assert_eq!(s.subdir, Some(PathBuf::from("plugins/x")));
        for bad in [
            "a",
            "a/",
            "/a/b",
            "-a/b",
            "a/b/../../etc",
            "a/b/./c",
            "a/b//c",
            "https://evil.example/a/b",
            "git@github.com:a/b",
            "a/b c",
            "../b",
            "file:///tmp/x",
        ] {
            assert!(Source::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_ref_that_reads_as_an_option_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        for bad in ["--upload-pack=touch /tmp/x", "-c", ""] {
            let err = fetch("https://github.com/a/b", Some(bad), &dir.path().join("c"))
                .expect_err("refused");
            assert!(err.to_string().contains("--ref"), "{err}");
        }
        assert!(!dir.path().join("c").exists(), "nothing was fetched");
    }

    #[test]
    fn the_callers_git_environment_never_reaches_git() {
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_SHALLOW_FILE",
        ] {
            assert!(gits(OsStr::new(name)), "{name}");
        }
        assert!(!gits(OsStr::new("GIT_TERMINAL_PROMPT")));
        assert!(!gits(OsStr::new("GIT_DIRT")));
    }

    #[test]
    fn a_subdir_or_manifest_leading_out_of_the_clone_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let clone = dir.path().join("clone");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(clone.join("plain")).expect("mkdir");
        std::fs::create_dir_all(&outside).expect("mkdir");
        std::fs::write(outside.join(manifest::FILE), "id = \"x\"").expect("write");
        std::fs::write(clone.join("plain").join(manifest::FILE), "id = \"x\"").expect("write");
        std::os::unix::fs::symlink(&outside, clone.join("escape")).expect("link");
        std::fs::create_dir_all(clone.join("sneaky")).expect("mkdir");
        std::os::unix::fs::symlink(
            outside.join(manifest::FILE),
            clone.join("sneaky").join(manifest::FILE),
        )
        .expect("link");
        std::fs::create_dir_all(clone.join("inner")).expect("mkdir");
        std::os::unix::fs::symlink(
            clone.join("plain").join(manifest::FILE),
            clone.join("inner").join(manifest::FILE),
        )
        .expect("link");
        assert!(root_in(&clone, Some(Path::new("plain"))).is_ok());
        assert!(root_in(&clone, Some(Path::new("escape"))).is_err());
        assert!(root_in(&clone, Some(Path::new("sneaky"))).is_err());
        assert!(
            root_in(&clone, Some(Path::new("inner"))).is_err(),
            "a manifest that is a link, even inside"
        );
        assert!(root_in(&clone, Some(Path::new("../outside"))).is_err());
        assert!(root_in(&clone, None).is_err(), "no manifest at the top");
    }

    #[test]
    fn a_manifest_over_the_bound_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let at = dir.path().join(manifest::FILE);
        let big = format!(
            "id = \"a\"\n#{}\n",
            "x".repeat(manifest::MAX_BYTES as usize)
        );
        std::fs::write(&at, big).expect("write");
        let err = read_manifest(&at).expect_err("refused");
        assert!(err.to_string().contains("over"), "{err}");
        std::fs::write(&at, "id = \"a\"\n").expect("write");
        assert_eq!(read_manifest(&at).expect("read"), "id = \"a\"\n");
    }

    #[test]
    fn the_preview_shows_every_command_and_the_powers() {
        let m = Manifest::parse(
            "id = \"p\"\nversion = \"1\"\nmin_herdr_version = \"9.0\"\n\
             [[build]]\ncommand = [\"sh\", \"b.sh\"]\n\
             [[actions]]\nid = \"a\"\ncommand = [\"x\", \"\\u001b[2Jhid\\u202Eden\\u2028line\"]\n\
             [[panes]]\nid = \"v\"\ncommand = [\"v\"]\n\
             [[events]]\non = \"pane.closed\"\ncommand = [\"e\"]\n\
             [[startup]]\ncommand = [\"s\", \"--daemon\"]\n",
        )
        .expect("parses");
        let text = preview(&m, "o/r", Some("abc123"));
        for line in [
            "plugin   p 1\n",
            "from     o/r\n",
            "commit   abc123 (recorded; your yes covers the manifest below)\n",
            "build    [\"sh\", \"b.sh\"]\n",
            "action   a [\"x\", \"\\u{1b}[2Jhid\\u{202e}den\\u{2028}line\"]\n",
            "pane     v (overlay) [\"v\"]\n",
            "event    pane.closed [\"e\"]\n",
            "startup  [\"s\", \"--daemon\"]\n",
            "warning  it asks for herdr 9.0",
        ] {
            assert!(text.contains(line), "{line:?} missing from\n{text}");
        }
        for hidden in ['\u{1b}', '\u{202e}', '\u{2028}'] {
            assert!(!text.contains(hidden), "{hidden:?} reached the terminal");
        }
        assert!(
            !text.lines().any(|l| l.starts_with("manifest")),
            "no hash line"
        );
        assert!(text.ends_with(&format!("{POWERS}\n")));
        assert!(preview(&m, "l", None).contains("a linked folder runs whatever it holds"));
    }

    #[test]
    fn a_local_install_builds_and_records_consent_and_rm_takes_it_away() {
        let dir = tempfile::tempdir().expect("dir");
        let upstream = repo(dir.path(), "t.plugin");
        let (_keep, home) = home();
        install(&home, &upstream, None).expect("installs");
        let root = home.dir_of("t.plugin");
        let built = std::fs::read_to_string(root.join("built.txt")).expect("the build ran");
        assert!(built.contains("HERDR_PLUGIN_ID=t.plugin"), "{built}");
        assert!(!built.lines().any(|l| l.starts_with("QOOP_")), "{built}");
        let plugin = admitted(&home, "t.plugin").expect("admitted");
        assert_eq!(plugin.consent.source, "t/p/sub");
        assert_eq!(plugin.consent.sha.as_ref().map(String::len), Some(40));
        assert!(plugin.consent.manifest.starts_with("id = \"t.plugin\""));
        let leftovers: Vec<_> = std::fs::read_dir(&home.root)
            .expect("read")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".install-"))
            .collect();
        assert!(leftovers.is_empty(), "the scratch clone is gone");

        let err = install(&home, &upstream, None).expect_err("already here");
        assert!(err.to_string().contains("rm t.plugin` first"), "{err}");
        assert!(
            admitted(&home, "t.plugin").is_ok(),
            "the refusal changed nothing"
        );

        assert!(home.state_of("t.plugin").is_dir());
        assert!(home.config_of("t.plugin").is_dir());
        rm(&home, "t.plugin").expect("removed");
        assert!(!root.exists());
        assert!(!home.state_of("t.plugin").exists(), "its state goes");
        assert!(home.config_of("t.plugin").is_dir(), "its config stays");
        assert!(admitted(&home, "t.plugin").is_err());
    }

    #[test]
    fn a_ref_pins_the_checkout_and_the_commit_is_recorded() {
        let dir = tempfile::tempdir().expect("dir");
        let upstream = repo(dir.path(), "t.pinned");
        git_in(&upstream, &["tag", "v1"]);
        let tagged = git(Some(&upstream), &["rev-parse", "HEAD"].map(OsStr::new)).expect("sha");
        std::fs::write(upstream.join("later.txt"), "after the tag").expect("write");
        git_in(&upstream, &["add", "."]);
        git_in(&upstream, &["commit", "--quiet", "-m", "later"]);
        let (_keep, home) = home();
        install(&home, &upstream, Some("v1")).expect("ok");
        let plugin = admitted(&home, "t.pinned").expect("admitted");
        assert_eq!(plugin.consent.sha.as_deref(), Some(tagged.as_str()));
        let (_other, fresh) = self::home();
        let err = install(&fresh, &upstream, Some("nope")).expect_err("no ref");
        assert!(err.to_string().contains("has no ref"), "{err}");
    }

    #[test]
    fn another_platform_is_refused_before_anything_runs() {
        let dir = tempfile::tempdir().expect("dir");
        let upstream = repo_with(
            dir.path(),
            "id = \"t.win\"\nplatforms = [\"windows\"]\n\
             [[build]]\ncommand = [\"sh\", \"-c\", \"touch built\"]\n",
        );
        let (_keep, home) = home();
        let err = install(&home, &upstream, None).expect_err("refused");
        assert!(err.to_string().contains("does not run on"), "{err}");
        assert!(!home.dir_of("t.win").exists());
    }

    #[test]
    fn a_changed_manifest_or_a_lost_ledger_refuses() {
        let dir = tempfile::tempdir().expect("dir");
        let upstream = repo(dir.path(), "t.change");
        let (_keep, home) = home();
        install(&home, &upstream, None).expect("ok");
        let manifest_at = home.dir_of("t.change").join(manifest::FILE);
        let original = std::fs::read(&manifest_at).expect("read");
        std::fs::write(
            &manifest_at,
            [original.as_slice(), b"\n# one more line\n"].concat(),
        )
        .expect("edit");
        let err = admitted(&home, "t.change").err().expect("refused");
        assert!(err.contains("manifest changed since you agreed"), "{err}");

        std::fs::write(&manifest_at, &original).expect("restore");
        assert!(admitted(&home, "t.change").is_ok());
        std::fs::write(home.ledger(), b"not json").expect("break");
        let err = admitted(&home, "t.change").err().expect("refused");
        assert!(err.contains("no consent"), "{err}");
    }

    #[test]
    fn a_failed_build_records_no_consent() {
        let dir = tempfile::tempdir().expect("dir");
        let upstream = repo_with(
            dir.path(),
            "id = \"t.broken\"\n[[build]]\ncommand = [\"sh\", \"-c\", \"exit 3\"]\n",
        );
        let (_keep, home) = home();
        let err = install(&home, &upstream, None).expect_err("the build failed");
        assert!(err.to_string().contains("rm t.broken` clears it"), "{err}");
        assert!(Ledger::open(&home.ledger()).plugins.is_empty());
        let refused = admitted(&home, "t.broken").err().expect("refused");
        assert!(refused.contains("no consent"), "{refused}");
        rm(&home, "t.broken").expect("cleared");
    }

    #[test]
    fn rm_of_a_link_leaves_its_folder_alone() {
        let dir = tempfile::tempdir().expect("dir");
        let live = folder(dir.path(), "live", "id = \"t.live\"\n");
        std::fs::write(live.join("keep.txt"), "mine").expect("file");
        let (_keep, home) = home();
        link(&home, &live, true).expect("linked");
        let plugin = admitted(&home, "t.live").expect("admitted");
        assert_eq!(plugin.consent.sha, None);
        assert_eq!(plugin.root, live.canonicalize().expect("canonical"));
        assert!(
            link(&home, &live, true).is_err(),
            "a second link is refused"
        );

        rm(&home, "t.live").expect("removed");
        assert!(std::fs::symlink_metadata(home.dir_of("t.live")).is_err());
        assert_eq!(
            std::fs::read_to_string(live.join("keep.txt")).expect("still there"),
            "mine"
        );
        assert!(live.join(manifest::FILE).is_file());
    }

    #[test]
    fn a_folder_inside_the_plugins_home_cannot_be_linked() {
        let (_keep, home) = home();
        std::fs::create_dir_all(&home.root).expect("mkdir");
        let inner = folder(&home.root, ".state", "id = \"t.inner\"\n");
        let err = link(&home, &inner, true).expect_err("refused");
        assert!(err.to_string().contains("inside qoop's own"), "{err}");
    }

    #[test]
    fn a_link_whose_folder_is_gone_still_shows() {
        let dir = tempfile::tempdir().expect("dir");
        let live = folder(dir.path(), "gone", "id = \"t.gone\"\n");
        let (_keep, home) = home();
        link(&home, &live, true).expect("linked");
        std::fs::remove_dir_all(&live).expect("rm");
        assert_eq!(installed(&home), ["t.gone"]);
        assert!(admitted(&home, "t.gone").is_err());
        rm(&home, "t.gone").expect("the dangling link goes");
        assert!(installed(&home).is_empty());
    }

    #[test]
    fn an_action_resolves_to_the_plugin_whose_id_it_ends_after() {
        let dir = tempfile::tempdir().expect("dir");
        let (_keep, home) = home();
        let a = folder(
            dir.path(),
            "a",
            "id = \"a\"\n[[actions]]\nid = \"x\"\ncommand = [\"x\"]\n",
        );
        let ab = folder(
            dir.path(),
            "ab",
            "id = \"a.b\"\n[[actions]]\nid = \"c\"\ncommand = [\"c\"]\n",
        );
        link(&home, &a, true).expect("a");
        link(&home, &ab, true).expect("a.b");
        assert_eq!(resolve(&home, "a.b.c"), Some(("a.b".into(), "c".into())));
        assert_eq!(resolve(&home, "a.x"), Some(("a".into(), "x".into())));
        assert_eq!(resolve(&home, "a.b"), Some(("a".into(), "b".into())));
        assert_eq!(resolve(&home, "z.x"), None);
        assert_eq!(resolve(&home, "a."), None);
        let err = run_action(&home, "a.b.c").expect_err("not yet");
        assert!(err.to_string().contains("not available yet"), "{err}");
    }

    #[test]
    fn rm_refuses_what_is_not_a_plugin_id() {
        let (_keep, home) = home();
        for bad in [
            "..",
            "../x",
            "a/b",
            "",
            ".bin",
            "consent.json",
            "consent.lock",
        ] {
            assert!(rm(&home, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn qoops_own_variables_are_scrubbed() {
        let mut cmd = Command::new("true");
        cmd.env("QOOP_TOKEN", "secret")
            .env("QORO_RUNTIME_DIR", "/r")
            .env("CLAUDECODE", "1")
            .env("KEEP_ME", "yes");
        scrub(&mut cmd);
        let envs: BTreeMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(OsStr::to_owned)))
            .collect();
        assert_eq!(envs["QOOP_TOKEN"], None, "removed");
        assert_eq!(envs["QORO_RUNTIME_DIR"], None, "removed");
        assert_eq!(envs["CLAUDECODE"], None, "a parent agent's, removed");
        assert_eq!(envs["KEEP_ME"], Some(OsString::from("yes")));
    }

    #[test]
    fn two_ledger_writers_both_land() {
        let (_keep, home) = home();
        let consent = |n: usize| Consent {
            sha: None,
            manifest: format!("id = \"p{n}\""),
            source: "s".into(),
        };
        std::thread::scope(|s| {
            for n in 0..8 {
                let home = &home;
                s.spawn(move || {
                    Ledger::update(home, |l| {
                        l.plugins.insert(format!("p{n}"), consent(n));
                    })
                    .expect("written");
                });
            }
        });
        assert_eq!(Ledger::open(&home.ledger()).plugins.len(), 8);
    }

    #[test]
    fn a_ledger_that_does_not_parse_is_kept_aside_not_written_over() {
        let (_keep, home) = home();
        std::fs::create_dir_all(&home.root).expect("mkdir");
        std::fs::write(home.ledger(), b"{ half written").expect("break");
        Ledger::update(&home, |l| {
            l.plugins.insert(
                "p".into(),
                Consent {
                    sha: None,
                    manifest: "id = \"p\"".into(),
                    source: "s".into(),
                },
            );
        })
        .expect("written");
        let aside: Vec<_> = std::fs::read_dir(&home.root)
            .expect("read")
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("consent.json.bad-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            std::fs::read(aside[0].path()).expect("read"),
            b"{ half written"
        );
        assert_eq!(Ledger::open(&home.ledger()).plugins.len(), 1);
    }
}

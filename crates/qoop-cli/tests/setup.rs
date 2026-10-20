//! qass's setup checklist (`skills/qass/setup.sh`), run as the install leaves it: each box
//! ticks only from what is really there, a file, a config value, a login, never from a word.
//!
//! Each test has a home of its own (repos, an ssh config, qoop's folders), installs the skill
//! there with the real binary, and runs the installed script. A stub `qoop` first on PATH answers
//! what needs a daemon or the phone part (`ls`, `agents`, `devices`, `new`) from files the test
//! writes, and hands everything else to the real binary.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const STUB: &str = r#"#!/bin/sh
d="$SETUP_STUB"
case "$1 ${2:-}" in
"ls --json") cat "$d/ls.json" 2>/dev/null || echo '[]' ;;
"agents --json") cat "$d/agents.json" 2>/dev/null || echo '[]' ;;
"task show")
    [ -f "$d/show-$3.json" ] || { echo "qoop: no board item $3" >&2; exit 1; }
    cat "$d/show-$3.json" ;;
"task board")
    [ -f "$d/board.rc" ] && exit "$(cat "$d/board.rc")"
    cat "$d/board.json" 2>/dev/null || echo '[]' ;;
"devices ls")
    [ -f "$d/devices" ] || { echo "qoop: this build has no phone support" >&2; exit 1; }
    cat "$d/devices" ;;
"new --space") echo "$*" >> "$d/new.log"; exit "$(cat "$d/new.rc" 2>/dev/null || echo 0)" ;;
*) exec "$SETUP_REAL" "$@" ;;
esac
"#;

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("qs")
            .tempdir_in("/tmp")
            .expect("temp dir");
        let home = Home { dir };
        for d in ["home/.ssh", "home/code", "c", "d", "rt", "stub", "bin"] {
            std::fs::create_dir_all(home.at(d)).expect("dir");
        }
        std::fs::set_permissions(home.at("rt"), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        home.write("bin/qoop", STUB);
        std::fs::set_permissions(home.at("bin/qoop"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let (ok, out, err) = home.qoop(&["skills", "install"]);
        assert!(ok, "skills install: {out} {err}");
        home
    }

    /// Paths as the script sees them: real paths (/tmp is /private/tmp on a Mac).
    fn at(&self, rel: &str) -> PathBuf {
        self.dir.path().canonicalize().expect("real path").join(rel)
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.at(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("write");
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
        let mut cmd = qoop_testkit::sandboxed(program);
        cmd.env("HOME", self.at("home"))
            .env("XDG_CONFIG_HOME", self.at("c"))
            .env("XDG_DATA_HOME", self.at("d"))
            .env("QOOP_RUNTIME_DIR", self.at("rt"))
            .env("SETUP_STUB", self.at("stub"))
            .env("SETUP_REAL", env!("CARGO_BIN_EXE_qoop"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.at("bin").display()),
            );
        cmd
    }

    fn output(mut cmd: std::process::Command) -> (bool, String, String) {
        let out = cmd.output().expect("run");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn qoop(&self, args: &[&str]) -> (bool, String, String) {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_qoop"));
        cmd.args(args);
        Self::output(cmd)
    }

    /// The installed script, as qass runs it.
    fn setup(&self, args: &[&str]) -> (bool, String, String) {
        let script = self.at("d/qoop/qass/.claude/skills/qass/setup.sh");
        let mut cmd = self.command(script);
        cmd.args(args);
        Self::output(cmd)
    }

    /// The installed script with `input` on its standard input.
    fn setup_in(&self, args: &[&str], input: &str) -> (bool, String, String) {
        use std::io::Write;
        let script = self.at("d/qoop/qass/.claude/skills/qass/setup.sh");
        let mut cmd = self.command(script);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn status(&self) -> String {
        let (ok, out, err) = self.setup(&["status"]);
        assert!(ok, "status: {out} {err}");
        out
    }

    /// The line of box `n`.
    fn item(&self, n: u8) -> String {
        let out = self.status();
        out.lines()
            .find(|l| l.get(4..6) == Some(&format!("{n} ")))
            .unwrap_or_else(|| panic!("no item {n} in {out}"))
            .to_string()
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = self.command("git");
        cmd.arg("-C").arg(dir).args(args);
        let (ok, out, err) = Self::output(cmd);
        assert!(ok, "git {args:?}: {out} {err}");
    }

    fn repo(&self, name: &str, files: &[&str]) -> PathBuf {
        let dir = self.at(&format!("home/code/{name}"));
        std::fs::create_dir_all(&dir).expect("dir");
        self.git(&dir, &["init", "-q"]);
        self.git(
            &dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        );
        for f in files {
            std::fs::write(dir.join(f), "").expect("file");
        }
        dir
    }

    /// Three projects and one worktree, as on a laptop.
    fn repos(&self) -> (PathBuf, PathBuf, PathBuf) {
        let api = self.repo("api", &["Cargo.toml"]);
        let web = self.repo("web", &["package.json", "bun.lock"]);
        let tool = self.repo("py tool", &["pyproject.toml"]);
        let wt = self.at("home/code/api-wt");
        self.git(
            &api,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().expect("utf8"),
                "-b",
                "wt",
            ],
        );
        (api, web, tool)
    }

    fn set_up(&self, repo: &Path) {
        for f in ["workflow.md", "project.md"] {
            std::fs::create_dir_all(repo.join(".qoop")).expect("dir");
            std::fs::write(repo.join(".qoop").join(f), "x\n").expect("write");
        }
    }
}

fn s(p: &Path) -> &str {
    p.to_str().expect("utf8")
}

const SSH: &str = "Host api-1 api\n  HostName api-1.example.com\n  User deploy\n\
                   Host=build\nHost github.com\n  User git\nHost *\n  ServerAliveInterval 30\n\
                   Host -oProxyCommand=x\n";

#[test]
fn projects_are_the_main_checkouts_newest_first_with_their_stack() {
    let home = Home::new();
    let (api, web, tool) = home.repos();
    let (ok, out, err) = home.setup(&["projects"]);
    assert!(ok, "{err}");
    for (repo, stack) in [(&api, "rust"), (&web, "bun"), (&tool, "python")] {
        let line = out
            .lines()
            .find(|l| l.contains(&format!("{}  ", s(repo))))
            .unwrap_or_else(|| panic!("{} missing from {out}", s(repo)));
        assert!(line.contains(&format!("  {stack}  ")), "{line}");
        assert!(line.contains(".qoop/ no"), "{line}");
    }
    assert!(
        !out.contains("api-wt"),
        "a worktree is not a project: {out}"
    );
    // Newest commit first: a later commit in tool puts it on top.
    let mut later = home.command("git");
    later
        .arg("-C")
        .arg(&tool)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["commit", "-q", "--allow-empty", "-m", "later"])
        .env("GIT_COMMITTER_DATE", "2099-01-01T00:00:00");
    assert!(Home::output(later).0);
    let (_, out, _) = home.setup(&["projects"]);
    assert!(
        out.lines().next().is_some_and(|l| l.contains(s(&tool))),
        "{out}"
    );
    // A folder of repos elsewhere, by name.
    let other = home.at("home/elsewhere/x");
    std::fs::create_dir_all(&other).expect("dir");
    home.git(&other, &["init", "-q"]);
    let (_, out, _) = home.setup(&["projects", s(&home.at("home/elsewhere"))]);
    assert!(out.contains(s(&other)) && !out.contains(s(&api)), "{out}");
}

#[test]
fn a_home_with_no_project_folder_says_so() {
    let home = Home::new();
    let _ = std::fs::remove_dir_all(home.at("home/code"));
    let out = home.status();
    assert!(
        home.item(2)
            .contains("0 found (no ~/code, ~/src or such folder here): none picked"),
        "{out}"
    );
}

#[test]
fn a_fresh_home_has_every_box_open_and_status_writes_nothing() {
    let home = Home::new();
    home.repos();
    home.write("home/.ssh/config", SSH);
    let out = home.status();
    assert!(out.contains("[ ] 1 "), "{out}");
    assert!(
        home.item(2).contains("3 found in ~/code: none picked"),
        "{out}"
    );
    // api-1, api and build; not github.com, `*` or the odd name.
    assert!(home.item(3).contains("3 found, 3 not decided"), "{out}");
    assert!(home.item(4).contains("after your projects"), "{out}");
    assert!(home.item(5).contains("not in this build"), "{out}");
    assert!(out.contains("[ ] 6 "), "{out}");
    assert!(out.ends_with("Next: type 1 (under a minute).\n"), "{out}");
    assert!(
        out.contains("/qass setup <folder>") && out.contains("skip <n>"),
        "{out}"
    );
    let state = home.at("d/qoop/qass/setup");
    assert!(!state.exists(), "status wrote the state file");
    let (_, servers, _) = home.setup(&["servers"]);
    assert!(
        servers.contains("-oProxyCommand=x  ignored: odd name"),
        "{servers}"
    );
    assert!(!servers.contains("github.com"), "{servers}");
    assert!(home.setup(&["seen"]).0);
    assert_eq!(std::fs::read_to_string(&state).expect("state"), "seen\n");
}

#[test]
fn settings_tick_once_saved_and_clean() {
    let home = Home::new();
    assert!(home.item(1).starts_with("[ ]"));
    assert!(home.qoop(&["config", "set", "qass.agent", "claude"]).0);
    assert!(home.item(1).starts_with("[x]"), "{}", home.status());
    let path = home.at("c/qoop/config.toml");
    let body = std::fs::read_to_string(&path).expect("config");
    std::fs::write(&path, format!("{body}\nprefix = \"nonsense\"\n")).expect("write");
    assert!(
        home.item(1).starts_with("[ ]"),
        "an unusable line: {}",
        home.status()
    );
}

#[test]
fn projects_tick_when_every_picked_one_has_both_files() {
    let home = Home::new();
    let (api, _, tool) = home.repos();
    for p in [&api, &tool] {
        let (ok, _, err) = home.setup(&["pick", s(p)]);
        assert!(ok, "{err}");
    }
    assert!(home.setup(&["pick", s(&api)]).0);
    let state = std::fs::read_to_string(home.at("d/qoop/qass/setup")).expect("state");
    assert_eq!(state.matches("pick ").count(), 2, "{state}");
    home.set_up(&api);
    let line = home.item(2);
    assert!(
        line.starts_with("[ ]") && line.contains("not written: py tool"),
        "{line}"
    );
    home.set_up(&tool);
    assert!(home.item(2).starts_with("[x]"), "{}", home.status());
    std::fs::write(tool.join(".qoop/project.md"), "").expect("empty");
    assert!(
        home.item(2).starts_with("[ ]"),
        "an empty file is not set up"
    );
    assert!(home.setup(&["drop", s(&tool)]).0);
    assert!(home.item(2).starts_with("[x]"), "{}", home.status());

    let wt = home.at("home/code/api-wt");
    let (ok, _, err) = home.setup(&["pick", s(&wt)]);
    assert!(!ok && err.contains(&format!("pick {}", s(&api))), "{err}");
    let (ok, _, _) = home.setup(&["pick", "code/api"]);
    assert!(!ok, "a relative path");
    let (ok, _, _) = home.setup(&["pick", s(&home.at("home"))]);
    assert!(!ok, "the home folder");
    // A bracket in a path is that path, never a pattern matching a sibling.
    let bracket = home.repo("app[1]", &[]);
    let sibling = home.repo("app1", &[]);
    home.set_up(&sibling);
    assert!(home.setup(&["pick", s(&bracket)]).0);
    let line = home.item(2);
    assert!(line.starts_with("[ ]") && line.contains("app[1]"), "{line}");
    assert!(home.setup(&["drop", s(&bracket)]).0);
    let (ok, _, _) = home.setup(&["pick", &format!("{}\nskip-host api", s(&api))]);
    assert!(!ok, "a newline");
    let state = std::fs::read_to_string(home.at("d/qoop/qass/setup")).expect("state");
    assert!(!state.contains("skip-host"), "{state}");
}

#[test]
fn servers_tick_only_from_a_remote_a_recorded_space_or_a_skip() {
    let home = Home::new();
    home.write("home/.ssh/config", SSH);
    // A space that only shares a name proves nothing.
    home.write(
        "stub/ls.json",
        r#"[{"id":"1","name":"build","state":"awake"}]"#,
    );
    assert!(home.item(3).contains("3 not decided"), "{}", home.status());
    // A failed `qoop new` records nothing.
    home.write("stub/new.rc", "1");
    assert!(!home.setup(&["ssh-space", "build"]).0);
    assert!(home.item(3).contains("3 not decided"));
    home.write("stub/new.rc", "0");
    assert!(home.setup(&["ssh-space", "build"]).0);
    let log = std::fs::read_to_string(home.at("stub/new.log")).expect("log");
    assert!(
        log.ends_with("new --space build --name build -- ssh build\n"),
        "{log}"
    );
    assert!(home.item(3).contains("2 not decided"), "{}", home.status());
    assert!(!home.setup(&["ssh-space", "-oProxyCommand=x"]).0);
    // A remote named after the host, and one whose target is the other alias.
    let remotes = home.at("c/qoop/remotes.json");
    let add = |name: &str, target: &str| {
        let mut list: Vec<serde_json::Value> = std::fs::read_to_string(&remotes)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        list.push(serde_json::json!({"name": name, "target": target}));
        std::fs::write(&remotes, serde_json::to_string(&list).expect("json")).expect("write");
    };
    add("api-1", "x.example.com");
    let (_, ls, _) = home.qoop(&["remote", "ls"]);
    assert!(
        ls.contains("api-1  x.example.com"),
        "the remote file is the real one: {ls}"
    );
    assert!(home.item(3).contains("1 not decided"), "{}", home.status());
    add("other", "nobody@elsewhere");
    assert!(
        home.item(3).contains("1 not decided"),
        "an unrelated remote ticks none"
    );
    add("r", "api");
    assert!(home.item(3).starts_with("[x]"), "{}", home.status());
}

#[test]
fn a_remote_counts_by_user_and_hostname_or_by_the_host_after_an_at() {
    let home = Home::new();
    home.write(
        "home/.ssh/config",
        "Host one\n  HostName one.example.com\n  User deploy\nHost two\n",
    );
    home.write(
        "c/qoop/remotes.json",
        r#"[{"name":"a","target":"deploy@one.example.com"},{"name":"b","target":"me@two"}]"#,
    );
    assert!(home.item(3).starts_with("[x]"), "{}", home.status());
    // A host no remote reaches stays open until he skips it, and the skip is what ticks it.
    home.write("home/.ssh/config", "Host two\nHost three\n");
    assert!(home.item(3).contains("1 not decided"), "{}", home.status());
    assert!(home.setup(&["skip-host", "three"]).0);
    let state = std::fs::read_to_string(home.at("d/qoop/qass/setup")).expect("state");
    assert_eq!(state, "skip-host three\n");
    assert!(home.item(3).starts_with("[x]"), "{}", home.status());
    assert!(!home.setup(&["skip-host", "bad name"]).0);
}

/// Item 4 asks nothing: it shows, for each picked project, whether qoop counts it the
/// person's own repo (its GitHub origin's owner is the login `gh` keeps, any case).
#[test]
fn what_i_may_do_shows_own_repo_for_each_picked_project() {
    let home = Home::new();
    let (api, web, _) = home.repos();
    let line = home.item(4);
    assert!(
        line.starts_with("[-]") && line.contains("after your projects"),
        "{line}"
    );
    home.git(
        &api,
        &["remote", "add", "origin", "git@github.com:Me/api.git"],
    );
    home.git(
        &web,
        &["remote", "add", "origin", "https://github.com/acme/web"],
    );
    assert!(home.setup(&["pick", s(&api)]).0 && home.setup(&["pick", s(&web)]).0);
    // No `gh` on the path: nothing is anyone's own.
    let line = home.item(4);
    assert!(line.contains("own repo: api no, web no"), "{line}");
    home.write("bin/gh", "#!/bin/sh\necho me\n");
    std::fs::set_permissions(home.at("bin/gh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let line = home.item(4);
    assert!(
        line.starts_with("[x]") && line.contains("own repo: api yes, web no"),
        "{line}"
    );
    assert!(!home.status().contains("policy"));
}

#[test]
fn the_phone_ticks_from_a_login_or_a_paired_device_never_from_a_status_line() {
    let home = Home::new();
    let hub = "c/qoop/hub.json";
    for (body, signed_in) in [
        (
            "{\n  \"url\": \"https://qoop.si\",\n  \"account\": \"a@b.c\"\n}",
            true,
        ),
        (r#"{"url":"x","account":"a\"bé"}"#, true),
        (r#"{"url":"x","account":null}"#, false),
        (r#"{"url":"x","account":"a@b.c","unlinking":true}"#, false),
        (r#"{"url":"x","account":"a@b.c","#, false),
        (r#"{"url":"x","account":"a@b.c",BROKEN}"#, false),
        (r#"{"account":"a\q"}"#, false),
        (r#"{"account":"a@b.c","x":01}"#, false),
        (r#"{"account":"a@b.c","unlinking":true}"#, false),
        (r#"{"url":"x"}"#, false),
    ] {
        home.write(hub, body);
        let line = home.item(5);
        assert_eq!(line.starts_with("[x]"), signed_in, "{body}: {line}");
        assert!(
            !line.contains("a@b.c"),
            "the account is never printed: {line}"
        );
    }
    std::fs::remove_file(home.at(hub)).expect("rm");
    home.write(
        "stub/devices",
        "no phones paired. sign in on qoop.si/app after `qoop login`\ncopied to 0 machine(s)\n",
    );
    assert!(home.item(5).starts_with("[ ]"));
    home.write(
        "stub/devices",
        "laptop                   key ab:cd  paired 3 days ago, browser, expired\n",
    );
    assert!(home.item(5).starts_with("[ ]"), "an expired browser");
    home.write(
        "stub/devices",
        "pixel                    key ab:cd  paired 2 days ago\n",
    );
    assert!(home.item(5).starts_with("[x]"), "{}", home.status());
}

#[test]
fn a_skipped_box_is_never_shown_done_and_setup_ends_when_none_is_open() {
    let home = Home::new();
    let (api, _, _) = home.repos();
    assert!(home.qoop(&["config", "set", "qass.agent", "claude"]).0);
    assert!(home.setup(&["pick", s(&api)]).0);
    home.set_up(&api);
    for n in ["4", "5", "6"] {
        assert!(home.setup(&["skip", n]).0);
    }
    assert!(!home.setup(&["skip", "1"]).0, "settings cannot be skipped");
    let out = home.status();
    assert!(
        out.contains("[-] 4 ") && out.contains("[-] 5 ") && out.contains("[-] 6 "),
        "{out}"
    );
    assert!(
        out.contains("[-] 3 "),
        "no ssh config: nothing to decide: {out}"
    );
    assert!(out.ends_with("setup done\n"), "{out}");
    assert!(home.setup(&["unskip", "5"]).0);
    assert!(
        home.status().ends_with("Next: type 5.\n"),
        "the phone was not measured: no minutes"
    );
    assert!(home.setup(&["unskip", "4"]).0);
    assert!(home.item(4).starts_with("[x] 4 "), "nothing to answer");
    for (skip, unskip) in [("4", "6"), ("5", "6")] {
        assert!(home.setup(&["skip", skip]).0 && home.setup(&["unskip", unskip]).0);
    }
    assert!(
        home.status().ends_with("Next: type 6 (about 2 minutes).\n"),
        "the first job's measured minutes"
    );
}

#[test]
fn the_first_job_ticks_only_when_its_own_item_closed_verified() {
    let home = Home::new();
    let line = home.item(6);
    assert!(
        line.starts_with("[ ]") && line.contains("one tiny real task"),
        "{line}"
    );
    // `task show <KEY> --json`, pretty, as the daemon prints it: the task record first, then
    // the item; `task_after` puts the task record last, as a sorted printer would.
    let show = |key: &str, state: &str, outcome: Option<&str>, task_after: bool| {
        let outcome = outcome.map_or(String::new(), |o| format!(",\n    \"outcome\": \"{o}\""));
        let task = "  \"task\": {\n    \"id\": 1,\n    \"stage\": \"verifying\",\n    \"outcome\": \"verified\"\n  }";
        let item = format!(
            "  \"item\": {{\n    \"id\": 1,\n    \"key\": \"{key}\",\n    \"workspace\": 1,\n    \"title\": \"x\",\n    \"state\": \"{state}\"{outcome},\n    \"agents\": [],\n    \"activity\": []\n  }}"
        );
        let body = if task_after {
            format!("{{\n  \"events\": [],\n{item},\n  \"more\": false,\n{task}\n}}\n")
        } else {
            format!("{{\n{task},\n  \"events\": [],\n  \"more\": false,\n{item}\n}}\n")
        };
        home.write(&format!("stub/show-{key}.json"), &body);
    };
    assert!(!home.setup(&["job", "SHOP-9"]).0, "no such item");
    show("OLD-1", "done", Some("verified"), false);
    assert!(!home.setup(&["job", "OLD-1"]).0, "a job done before setup");
    show("OLD-2", "done", Some("failed"), false);
    assert!(
        !home.setup(&["job", "OLD-2"]).0,
        "a job that failed before setup"
    );
    assert!(!home.setup(&["job", "bad key"]).0);
    show("SHOP-1", "doing", None, false);
    assert!(home.setup(&["job", "SHOP-1"]).0);
    let line = home.item(6);
    assert!(
        line.starts_with("[ ]") && line.contains("SHOP-1 started"),
        "{line}"
    );
    for task_after in [false, true] {
        show("SHOP-1", "done", Some("failed"), task_after);
        assert!(
            home.item(6).starts_with("[ ]"),
            "a failed job is not a win, whatever its task record says"
        );
    }
    show("SHOP-1", "done", Some("verified"), true);
    assert!(home.item(6).starts_with("[x]"), "{}", home.status());
    // The item gone from the daemon's answer: open, never done.
    std::fs::remove_file(home.at("stub/show-SHOP-1.json")).expect("rm");
    assert!(home.item(6).starts_with("[ ]"));
    assert!(home.setup(&["skip", "6"]).0);
    assert!(home.item(6).starts_with("[-]"));
}

#[test]
fn undo_takes_back_only_what_setup_wrote_and_he_left_alone() {
    let home = Home::new();
    let (api, web, tool) = home.repos();
    let bracket = home.repo("app[1]", &[]);
    for p in [&api, &web, &tool, &bracket] {
        assert!(home.setup(&["pick", s(p)]).0);
    }
    // His own workflow.md, there before setup: never written over, never recorded.
    std::fs::create_dir_all(tool.join(".qoop")).expect("dir");
    std::fs::write(tool.join(".qoop/workflow.md"), "his\n").expect("his");
    let (ok, _, err) = home.setup_in(&["write", s(&tool.join(".qoop/workflow.md"))], "mine\n");
    assert!(!ok && err.contains("it is theirs"), "{err}");
    assert_eq!(
        std::fs::read_to_string(tool.join(".qoop/workflow.md")).expect("read"),
        "his\n"
    );
    for p in [&api, &web, &tool, &bracket] {
        let (ok, out, err) = home.setup_in(&["write", s(&p.join(".qoop/project.md"))], "facts\n");
        assert!(ok && out.starts_with("wrote "), "{err}");
    }
    for p in [&api, &web, &bracket] {
        assert!(
            home.setup_in(&["write", s(&p.join(".qoop/workflow.md"))], "steps\n")
                .0
        );
    }
    // Rewriting its own unchanged file is fine; one he changed is his.
    assert!(
        home.setup_in(&["write", s(&api.join(".qoop/workflow.md"))], "steps 2\n")
            .0
    );
    std::fs::write(web.join(".qoop/project.md"), "his words\n").expect("write");
    assert!(
        !home
            .setup_in(&["write", s(&web.join(".qoop/project.md"))], "x\n")
            .0
    );

    // Only a .qoop/ file of a picked project.
    let state = || std::fs::read_to_string(home.at("d/qoop/qass/setup")).expect("state");
    let before = state();
    assert!(!home.setup_in(&["write", s(&api.join("README.md"))], "x").0);
    let elsewhere = home.at("home/elsewhere/.qoop/workflow.md");
    assert!(
        !home.setup_in(&["write", s(&elsewhere)], "x").0,
        "not a picked project"
    );
    assert!(!elsewhere.exists());
    assert_eq!(before, state(), "a refused write records nothing");

    // A link at the old fixed temporary name leads nowhere: the outside file stays as it was.
    let outside = home.at("home/outside.txt");
    std::fs::write(&outside, "outside\n").expect("write");
    std::os::unix::fs::symlink(&outside, api.join(".qoop/workflow.md.setup-new")).expect("link");
    assert!(
        home.setup_in(&["write", s(&api.join(".qoop/workflow.md"))], "steps 3\n")
            .0
    );
    assert_eq!(
        std::fs::read_to_string(&outside).expect("read"),
        "outside\n"
    );
    std::fs::remove_file(api.join(".qoop/workflow.md.setup-new")).expect("rm link");
    // A way in through `sub/..` to a linked .qoop/ writes nothing outside.
    let linked = home.repo("linked", &[]);
    std::fs::create_dir_all(linked.join("sub")).expect("dir");
    let away = home.at("home/away");
    std::fs::create_dir_all(&away).expect("dir");
    std::os::unix::fs::symlink(&away, linked.join(".qoop")).expect("link");
    assert!(home.setup(&["pick", s(&linked)]).0);
    let through = format!("{}/sub/../.qoop/project.md", s(&linked));
    assert!(!home.setup_in(&["write", &through], "x\n").0);
    assert!(!away.join("project.md").exists());

    // He committed tool's project.md: his now, never rewritten, never undone.
    home.git(&tool, &["add", ".qoop/project.md"]);
    home.git(
        &tool,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "setup",
        ],
    );
    let (ok, _, err) = home.setup_in(&["write", s(&tool.join(".qoop/project.md"))], "facts\n");
    assert!(!ok && err.contains("it is theirs"), "{err}");
    // A repo whose git cannot answer keeps its files.
    let notes = home.repo("notes", &[]);
    assert!(home.setup(&["pick", s(&notes)]).0);
    assert!(
        home.setup_in(&["write", s(&notes.join(".qoop/project.md"))], "x\n")
            .0
    );
    std::fs::rename(notes.join(".git"), notes.join("git-gone")).expect("hide git");
    let (ok, out, err) = home.setup(&["undo"]);
    assert!(ok, "{err}");
    assert!(
        !api.join(".qoop").exists(),
        "api's files and folder are gone: {out}"
    );
    assert!(
        !bracket.join(".qoop").exists(),
        "a bracket in a path is that path: {out}"
    );
    assert!(!web.join(".qoop/workflow.md").exists(), "{out}");
    assert!(
        out.contains(&format!("kept {}/.qoop/project.md: changed", s(&web))),
        "{out}"
    );
    assert!(out.contains("in git (staged or committed)"), "{out}");
    assert!(out.contains("git did not answer"), "{out}");
    assert!(notes.join(".qoop/project.md").exists());
    assert_eq!(
        std::fs::read_to_string(tool.join(".qoop/workflow.md")).expect("read"),
        "his\n",
        "his file is untouched"
    );
    let after = state();
    assert!(
        !after.contains(&format!("pick {}\n", s(&api))),
        "nothing left of api: {after}"
    );
    assert!(after.contains(&format!("pick {}\n", s(&web))), "{after}");
    assert!(after.contains(&format!("pick {}\n", s(&tool))), "{after}");
}

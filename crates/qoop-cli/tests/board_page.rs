//! `qoop qass` puts the board page above qass in its tab, and that tab still sleeps.
//!
//! Against a live daemon through the real socket, in a sandbox: a stand-in `claude` that
//! exits at once (so qass's pane is a shell at its prompt) and qass's skill where `qoop qass`
//! looks for it. The board runs for real in its pane, on the daemon's own terminal.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse, Pane};

struct Daemon {
    child: Child,
    dir: PathBuf,
    path: String,
}

impl Daemon {
    fn start(dir: &Path) -> Self {
        Self::start_with(dir, "exit 0\n")
    }

    /// The same, with the stand-in `claude` running `body`.
    fn start_with(dir: &Path, body: &str) -> Self {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let skill = dir.join("d/qoop/qass/.claude/skills/qass");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "qass\n").unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_DATA_HOME", dir.join("d"))
            .env("PATH", &path)
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        Self {
            child,
            dir: dir.to_path_buf(),
            path,
        }
    }

    /// `qoop` from outside every terminal of this daemon.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .env("QOOP_RUNTIME_DIR", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_DATA_HOME", self.dir.join("d"))
            .env("PATH", &self.path)
            .output()
            .expect("run qoop");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The daemon's index: its rows and its split trees.
    fn index(&self) -> (Vec<AgentRow>, Vec<Pane>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (stream, inbuf) = qoop_runtime::client::connect_observer(
                &qoop_testkit::socket_in(&self.dir),
                "board-test",
            )
            .await
            .expect("connect");
            let mut c = qoop_testkit::TestClient::from_parts(stream, inbuf, 1);
            match c
                .control(
                    &ControlRequest::ListAgents { id: "index".into() },
                    qoop_testkit::PATIENCE,
                )
                .await
                .expect("an index")
            {
                ControlResponse::Agents { agents, splits, .. } => (agents, splits),
                other => panic!("expected the index, got {other:?}"),
            }
        })
    }

    /// Wait until `done` holds, saying `what` and `seen` when it never does.
    fn until(&self, what: &str, seen: &dyn Fn() -> String, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(
                Instant::now() < deadline,
                "{what} never happened: {}",
                seen()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn qoop_qass_puts_the_board_above_qass_and_the_tab_still_sleeps() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path());
    let (ok, out, err) = daemon.run(&["task", "add", "--space", "1", "check", "the", "board"]);
    assert!(ok, "task add: {out} {err}");

    let (ok, out, err) = daemon.run(&["qass"]);
    assert!(ok, "qoop qass: {out} {err}");
    assert!(!err.contains("board did not open"), "{err}");
    let qass: u32 = out
        .split_whitespace()
        .skip_while(|w| *w != "terminal")
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no terminal in {out:?}"));

    // One tab: the board first, on top, qass under it; the board page running in its pane.
    let mut board = 0;
    daemon.until(
        "the board above qass",
        &|| format!("{:?}", daemon.index()),
        || {
            let (rows, splits) = daemon.index();
            let Some(tree) = splits.iter().find(|t| t.leaves().contains(&qass)) else {
                return false;
            };
            let leaves = tree.leaves();
            board = leaves[0];
            let first = rows.iter().find(|r| r.terminal == board);
            matches!(
                tree,
                Pane::Split {
                    dir: qoop_runtime::control::SplitDir::H,
                    ..
                }
            ) && leaves == [board, qass]
                && first.is_some_and(|r| {
                    r.name.as_deref() == Some("board") && r.program.as_deref() == Some("qoop")
                })
        },
    );
    let read = || daemon.run(&["read", &board.to_string()]).1;
    daemon.until("the board drawn in its pane", &read, || {
        let screen = read();
        [
            "TODO",
            "DOING",
            "DONE",
            "check the board",
            "DEFAULT-1",
            "no age",
        ]
        .iter()
        .all(|w| screen.contains(w))
    });

    // The tab sleeps whole, board page and all: qoop's own page is no work a sleep loses.
    let mut said = String::new();
    daemon.until("qass's tab asleep", &|| String::new(), || {
        let (ok, out, err) = daemon.run(&["sleep", "--tab", &board.to_string()]);
        said = format!("{out}{err}");
        ok
    });
    assert!(!said.contains("running in it"), "{said}");
    let (rows, _) = daemon.index();
    for t in [board, qass] {
        assert!(
            rows.iter()
                .filter(|r| r.terminal == t)
                .all(|r| r.state == "sleeping"),
            "{t} is still awake after: {said}; {rows:?}"
        );
    }
}

/// `qoop task start` asks qass the way the board does, as nobody and with one line: while qass
/// waits on the owner it records nothing and types nothing (the enter would answer qass's
/// prompt); once qass listens, the key as stored reaches it.
#[test]
fn a_start_waits_while_qass_waits_on_the_owner_and_then_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let go = dir.path().join("go");
    let q = env!("CARGO_BIN_EXE_qoop");
    let daemon = Daemon::start_with(
        dir.path(),
        &format!(
            "'{q}' hook-report --state blocked --session-id s1\n\
             while [ ! -f '{go}' ]; do sleep 0.2; done\n\
             '{q}' hook-report --state idle --session-id s1\n\
             while read -r l; do echo \"HEARD:$l\"; done\n",
            go = go.display(),
        ),
    );
    let (ok, out, err) = daemon.run(&["task", "add", "--space", "1", "fix", "it"]);
    assert!(ok, "task add: {err}");
    let key = out.split_whitespace().next().unwrap().to_string();
    let (ok, out, err) = daemon.run(&["qass"]);
    assert!(ok, "qoop qass: {out} {err}");
    let qass: u32 = out
        .split_whitespace()
        .skip_while(|w| *w != "terminal")
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no terminal in {out:?}"));
    let state = || {
        daemon
            .index()
            .0
            .into_iter()
            .find(|r| r.terminal == qass)
            .map(|r| (r.agent, r.state))
    };
    daemon.until(
        "qass waiting on the owner",
        &|| format!("{:?}", state()),
        || state() == Some((true, "blocked".into())),
    );
    let board = || {
        let (_, out, _) = daemon.run(&["task", "board", "--json"]);
        out
    };
    let (ok, _, err) = daemon.run(&["task", "start", &key.to_lowercase()]);
    assert!(!ok && err.contains("qass waits on you"), "{err}");
    assert!(
        !board().contains("\"doing\""),
        "a refused start records nothing: {}",
        board()
    );

    std::fs::write(&go, "").unwrap();
    daemon.until("qass listening", &|| format!("{:?}", state()), || {
        state().is_some_and(|(_, s)| s == "idle")
    });
    let (ok, out, err) = daemon.run(&["task", "start", &key.to_lowercase()]);
    assert!(ok, "start: {out} {err}");
    assert!(
        out.contains(&format!("{key} started; qass is told")),
        "{out}"
    );
    let read = || daemon.run(&["read", &qass.to_string()]).1;
    daemon.until("the start in qass's terminal", &read, || {
        read().contains(&format!("HEARD:start {key}"))
    });
    assert!(board().contains("\"doing\""), "{}", board());
}

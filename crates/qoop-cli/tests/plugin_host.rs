//! `qoop plugin host` and the `herdr` shim against a live daemon, through the real sockets.
//!
//! Each test runs its own daemon and host in a private runtime directory with its own config
//! and data homes; nothing here reaches a person's daemon or a real herdr.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Stack {
    daemon: Child,
    host: Child,
    dir: tempfile::TempDir,
}

fn qoop(dir: &Path) -> Command {
    let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
    cmd.env("QOOP_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("SHELL", "/bin/sh");
    cmd
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{what} never happened");
}

impl Stack {
    /// A stack with one plugin that does nothing: a host with no plugin to serve leaves.
    fn start() -> Self {
        Self::start_with(|dir| {
            let base = "id = \"t.base\"\n";
            plant(
                &dir.join("data"),
                "t.base",
                base,
                base,
                &mut json!({"plugins": {}}),
            );
        })
    }

    /// The same, with `prepare` run on the test's folder after the daemon is up and before
    /// the host starts.
    fn start_with(prepare: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let daemon = qoop(dir.path())
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        wait_for("the daemon's socket", || socket.exists());
        qoop_testkit::open_first_terminal(&socket);
        prepare(dir.path());
        let host = qoop(dir.path())
            .args(["plugin", "host"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the host");
        let stack = Self { daemon, host, dir };
        let herdr = stack.herdr_socket();
        wait_for("the host's socket", || UnixStream::connect(&herdr).is_ok());
        stack
    }

    fn herdr_socket(&self) -> PathBuf {
        qoop_testkit::socket_in(self.dir.path()).with_file_name("herdr.sock")
    }

    fn ask(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(self.herdr_socket()).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("timeout");
        let request = json!({"id": "t", "method": method, "params": params});
        stream
            .write_all(format!("{request}\n").as_bytes())
            .expect("write");
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("one json line")
    }

    fn shim(&self) -> PathBuf {
        self.dir.path().join("data/qoop/plugins/.bin/herdr")
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // This test's own processes, by their handles.
        let _ = self.host.kill();
        let _ = self.host.wait();
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[test]
fn the_host_answers_herdrs_reads_in_herdrs_shapes() {
    let stack = Stack::start();
    assert_eq!(
        stack.ask("ping", json!({}))["result"],
        json!({"type": "pong", "version": "0.9.5", "protocol": 22})
    );

    let list = stack.ask("pane.list", json!({}));
    assert_eq!(list["result"]["type"], "pane_list", "{list}");
    let pane = list["result"]["panes"][0].clone();
    let id = pane["pane_id"].as_str().expect("a pane id").to_string();
    assert!(id.starts_with('w') && id.contains(":p"), "{id}");
    assert!(pane["tab_id"].as_str().expect("tab").contains(":t"));
    assert_eq!(pane["agent_status"], "unknown", "a shell has no agent");

    let got = stack.ask("pane.get", json!({"pane_id": id}));
    assert_eq!(got["result"]["pane"]["pane_id"], id.as_str());
    let info = stack.ask("pane.process_info", json!({"pane_id": id}));
    let shell = &info["result"]["process_info"]["shell_pid"];
    assert!(shell.as_u64().is_some_and(|p| p > 1), "{info}");

    let tabs = stack.ask("tab.list", json!({}));
    assert_eq!(tabs["result"]["tabs"][0]["tab_id"], pane["tab_id"]);
    let spaces = stack.ask("workspace.list", json!({}));
    assert_eq!(
        spaces["result"]["workspaces"][0]["workspace_id"],
        pane["workspace_id"]
    );
    let snapshot = stack.ask("session.snapshot", json!({}));
    assert_eq!(snapshot["result"]["snapshot"]["version"], "0.9.5");
    assert_eq!(
        snapshot["result"]["snapshot"]["layouts"][0]["tab_id"],
        pane["tab_id"]
    );
    let layout = stack.ask("pane.layout", json!({"pane_id": id}));
    assert_eq!(
        layout["result"]["layout"]["panes"][0]["pane_id"],
        id.as_str()
    );

    let read = stack.ask("pane.read", json!({"pane_id": id, "source": "visible"}));
    assert_eq!(read["result"]["read"]["pane_id"], id.as_str(), "{read}");
    assert!(read["result"]["read"]["text"].is_string());

    let missing = stack.ask("pane.get", json!({"pane_id": "w1:p999"}));
    assert_eq!(missing["error"]["code"], "pane_not_found");
    let unknown = stack.ask("pane.report_agent", json!({}));
    assert_eq!(unknown["error"]["code"], "invalid_request");
    assert!(
        unknown["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("invalid request: unknown variant `pane.report_agent`")),
        "{unknown}"
    );
    assert_eq!(
        stack.ask("server.reload_config", json!({}))["result"],
        json!({"type": "config_reload", "status": "applied", "diagnostics": []})
    );
    let resized = stack.ask("pane.resize", json!({"pane_id": id, "direction": "left"}));
    assert_eq!(resized["result"]["type"], "pane_resize", "{resized}");
    assert_eq!(resized["result"]["resize"]["changed"], false);
    assert_eq!(resized["result"]["resize"]["pane_id"], id.as_str());

    // The id comes back as it was sent; one that is neither a string nor a number is refused.
    let mut stream = UnixStream::connect(stack.herdr_socket()).expect("connect");
    stream
        .write_all(b"{\"id\":\"5\",\"method\":\"ping\"}\n{\"id\":5,\"method\":\"ping\"}\n")
        .expect("write");
    let mut lines = BufReader::new(stream).lines();
    let numbered: Value =
        serde_json::from_str(&lines.next().expect("line").expect("read")).expect("json");
    assert_eq!(numbered["id"], "5");
    assert_eq!(numbered["result"]["type"], "pong");
    let odd: Value =
        serde_json::from_str(&lines.next().expect("line").expect("read")).expect("json");
    assert_eq!(odd["error"]["code"], "invalid_request", "{odd}");
    assert_eq!(odd["id"], "", "a refused id is answered as an empty one");
}

#[test]
fn a_subscriber_hears_a_pane_open_and_close() {
    let stack = Stack::start();
    let mut stream = UnixStream::connect(stack.herdr_socket()).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    let subscribe = json!({"id": "s", "method": "events.subscribe", "params": {"subscriptions": [
        {"type": "pane.created"}, {"type": "pane.closed"}
    ]}});
    stream
        .write_all(format!("{subscribe}\n").as_bytes())
        .expect("write");
    let mut lines = BufReader::new(stream).lines();
    let mut next = || -> Value {
        serde_json::from_str(&lines.next().expect("a line").expect("read")).expect("json")
    };
    assert_eq!(
        next(),
        json!({"id": "s", "result": {"type": "subscription_started"}})
    );

    let made = qoop(stack.dir.path())
        .arg("new")
        .output()
        .expect("qoop new");
    assert!(made.status.success(), "{made:?}");
    let terminal = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let created = next();
    assert_eq!(created["event"], "pane_created", "{created}");
    assert_eq!(created["data"]["type"], "pane_created");
    let pane_id = created["data"]["pane"]["pane_id"]
        .as_str()
        .expect("id")
        .to_string();
    assert!(
        pane_id.ends_with(&format!(":p{terminal}")),
        "{pane_id} {terminal}"
    );

    let closed = qoop(stack.dir.path())
        .args(["close", &terminal])
        .output()
        .expect("qoop close");
    assert!(closed.status.success(), "{closed:?}");
    let gone = next();
    assert_eq!(gone["event"], "pane_closed", "{gone}");
    assert_eq!(gone["data"]["pane_id"], pane_id.as_str());

    let mut other = UnixStream::connect(stack.herdr_socket()).expect("connect");
    let unknown = json!({"id": "u", "method": "events.subscribe", "params": {"subscriptions": [
        {"type": "pane.agent_status_changed", "pane_id": "w1:p999"}
    ]}});
    other
        .write_all(format!("{unknown}\n").as_bytes())
        .expect("write");
    let mut line = String::new();
    BufReader::new(other).read_line(&mut line).expect("read");
    let refused: Value = serde_json::from_str(&line).expect("json");
    assert_eq!(refused["error"]["code"], "pane_not_found", "{refused}");
}

#[test]
fn the_shim_is_herdr_for_a_plugin_and_nothing_else() {
    let stack = Stack::start();
    let shim = stack.shim();
    wait_for("the shim link", || shim.exists());
    let herdr = |args: &[&str]| {
        qoop_testkit::sandboxed(&shim)
            .args(args)
            .env("XDG_DATA_HOME", stack.dir.path().join("data"))
            .env("HERDR_SOCKET_PATH", stack.herdr_socket())
            .output()
            .expect("run the shim")
    };
    let version = herdr(&["--version"]);
    assert!(version.status.success());
    assert_eq!(String::from_utf8_lossy(&version.stdout), "herdr 0.9.5\n");

    let list = herdr(&["pane", "list"]);
    assert!(list.status.success(), "{list:?}");
    let answer: Value = serde_json::from_slice(&list.stdout).expect("json");
    assert_eq!(answer["result"]["type"], "pane_list");
    assert!(answer["result"]["panes"][0]["pane_id"].is_string());
    let first = answer["result"]["panes"][0]["pane_id"]
        .as_str()
        .expect("id")
        .to_string();
    let text = herdr(&[
        "pane",
        "read",
        &first,
        "--source",
        "recent-unwrapped",
        "--format",
        "text",
    ]);
    assert!(text.status.success(), "{text:?}");
    assert!(
        !String::from_utf8_lossy(&text.stdout).starts_with('{'),
        "--format prints the text itself: {text:?}"
    );

    let unknown = herdr(&["integration", "status"]);
    assert_eq!(unknown.status.code(), Some(1));
    let refused: Value = serde_json::from_slice(&unknown.stderr).expect("json on stderr");
    assert_eq!(refused["error"]["code"], "invalid_request");

    // The same binary under the name herdr anywhere else is plain qoop.
    let elsewhere = stack.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("mkdir");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qoop"), elsewhere.join("herdr")).expect("link");
    let plain = qoop_testkit::sandboxed(elsewhere.join("herdr"))
        .arg("--version")
        .env("XDG_DATA_HOME", stack.dir.path().join("data"))
        .output()
        .expect("run");
    assert!(
        !String::from_utf8_lossy(&plain.stdout).starts_with("herdr"),
        "{plain:?}"
    );
}

#[test]
fn the_host_leaves_when_the_daemon_does() {
    let mut stack = Stack::start();
    let _ = stack.daemon.kill();
    let _ = stack.daemon.wait();
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if stack.host.try_wait().expect("wait").is_some() {
            assert!(!stack.herdr_socket().exists(), "the socket went with it");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the host stayed after its daemon left");
}

/// A linked plugin in `data`, recorded in the consent ledger as `agreed` (its own manifest
/// unless the test wants a mismatch).
fn plant(data: &Path, id: &str, manifest: &str, agreed: &str, ledger: &mut Value) {
    let folder = data.join("folders").join(id);
    std::fs::create_dir_all(&folder).expect("mkdir");
    std::fs::write(folder.join("herdr-plugin.toml"), manifest).expect("manifest");
    let plugins = data.join("qoop/plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::os::unix::fs::symlink(&folder, plugins.join(id)).expect("link");
    ledger["plugins"][id] = json!({"sha": null, "manifest": agreed, "source": "link test"});
    std::fs::write(plugins.join("consent.json"), ledger.to_string()).expect("ledger");
}

#[test]
fn hooks_and_startup_run_with_the_herdr_env_and_only_with_consent() {
    let good = "id = \"t.hook\"\n\
                [[events]]\non = \"pane.created\"\n\
                command = [\"sh\", \"-c\", \"env > \\\"$HERDR_PLUGIN_STATE_DIR/event.txt\\\"\"]\n\
                [[startup]]\n\
                command = [\"sh\", \"-c\", \"echo up > \\\"$HERDR_PLUGIN_STATE_DIR/startup.txt\\\"; sleep 5\"]\n";
    let bad = good.replace("t.hook", "t.bad");
    let stack = Stack::start_with(|dir| {
        let data = dir.join("data");
        let mut ledger = json!({"plugins": {}});
        plant(&data, "t.hook", good, good, &mut ledger);
        plant(&data, "t.bad", &bad, "id = \"t.bad\"\n", &mut ledger);
    });
    let state = |id: &str, file: &str| {
        stack
            .dir
            .path()
            .join("data/qoop/plugins/.state")
            .join(id)
            .join(file)
    };
    wait_for("the startup command", || {
        state("t.hook", "startup.txt").exists()
    });

    let made = qoop(stack.dir.path())
        .arg("new")
        .output()
        .expect("qoop new");
    assert!(made.status.success(), "{made:?}");
    let terminal = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let event = state("t.hook", "event.txt");
    wait_for("the pane.created hook", || {
        std::fs::read_to_string(&event).is_ok_and(|e| e.contains("HERDR_PLUGIN_EVENT="))
    });
    let env = std::fs::read_to_string(&event).expect("read");
    for want in [
        "HERDR_PLUGIN_EVENT=pane_created".to_string(),
        "\"type\":\"pane_created\"".to_string(),
        format!("HERDR_PANE_ID=w1:p{terminal}"),
        "HERDR_PLUGIN_CONTEXT_JSON=".to_string(),
        "HERDR_PLUGIN_ID=t.hook".to_string(),
        "HERDR_ENV=1".to_string(),
    ] {
        assert!(env.contains(&want), "{want} missing from\n{env}");
    }
    assert!(!env.lines().any(|l| l.starts_with("QOOP_")), "{env}");

    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !state("t.bad", "event.txt").exists(),
        "a changed manifest never runs"
    );
    assert!(!state("t.bad", "startup.txt").exists());
    let log = std::fs::read_to_string(state("t.bad", "host.log")).unwrap_or_default();
    assert!(log.contains("manifest changed"), "{log}");
}

#[test]
fn the_host_follows_a_daemon_that_comes_back() {
    let mut stack = Stack::start();
    let _ = stack.daemon.kill();
    let _ = stack.daemon.wait();
    stack.daemon = qoop(stack.dir.path())
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon again");
    let socket = qoop_testkit::socket_in(stack.dir.path());
    wait_for("the new daemon's socket", || {
        UnixStream::connect(&socket).is_ok()
    });
    qoop_testkit::open_first_terminal(&socket);
    wait_for("the host to list the new daemon", || {
        stack.ask("pane.list", json!({}))["result"]["panes"]
            .as_array()
            .is_some_and(|p| !p.is_empty())
    });
    assert!(stack.host.try_wait().expect("wait").is_none(), "still here");
}

#[test]
fn startup_commands_end_when_the_host_leaves() {
    let startup = "id = \"t.long\"\n\
                   [[startup]]\n\
                   command = [\"sh\", \"-c\", \"echo $$ > \\\"$HERDR_PLUGIN_STATE_DIR/pid\\\"; exec sleep 60\"]\n";
    let mut stack = Stack::start_with(|dir| {
        plant(
            &dir.join("data"),
            "t.long",
            startup,
            startup,
            &mut json!({"plugins": {}}),
        );
    });
    let pid_file = stack.dir.path().join("data/qoop/plugins/.state/t.long/pid");
    wait_for("the startup command", || {
        std::fs::read_to_string(&pid_file).is_ok_and(|p| p.trim().parse::<i32>().is_ok())
    });
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("pid")
        .trim()
        .parse()
        .expect("number");
    // SAFETY: signal 0 only asks whether the process exists.
    let alive = || unsafe { libc::kill(pid, 0) } == 0;
    assert!(alive());
    let _ = stack.daemon.kill();
    let _ = stack.daemon.wait();
    wait_for("the host to leave", || {
        stack.host.try_wait().expect("wait").is_some()
    });
    wait_for("its startup command to end", || !alive());
}

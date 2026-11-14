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
        // Never followed by anything of qoop's own: only the shim reads it (Acceptance 6).
        .env("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock")
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
            // Kept, so a failing test can say what the host said.
            .stderr(std::fs::File::create(dir.path().join("host.err")).expect("host.err"))
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
        serde_json::from_str(&line).unwrap_or_else(|e| {
            let said =
                std::fs::read_to_string(self.dir.path().join("host.err")).unwrap_or_default();
            panic!("{method}: no answer ({e}); the host said:\n{said}")
        })
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
        {"type": "pane.created"}, {"type": "pane.exited"}
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
    assert_eq!(
        gone["event"], "pane_exited",
        "a terminal closed outside the API: {gone}"
    );
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

    // Called wrong: plain words and exit 2, as herdr's command line says it.
    let unknown = herdr(&["integration", "status"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).starts_with("unknown command: integration status")
    );
    // A refusal from the host: its envelope on stderr and exit 1.
    let refused = herdr(&["pane", "get", "w1:p999"]);
    assert_eq!(refused.status.code(), Some(1));
    let refused: Value = serde_json::from_slice(&refused.stderr).expect("json on stderr");
    assert_eq!(refused["error"]["code"], "pane_not_found");
    // The quiet commands print nothing on success.
    let quiet = herdr(&["pane", "send-text", &first, ""]);
    assert!(
        quiet.status.success() && quiet.stdout.is_empty(),
        "{quiet:?}"
    );
    // No host at the socket named: herdr's `server_not_running`.
    let gone = qoop_testkit::sandboxed(&shim)
        .args(["pane", "list"])
        .env("XDG_DATA_HOME", stack.dir.path().join("data"))
        .env("HERDR_SOCKET_PATH", stack.dir.path().join("nobody.sock"))
        .output()
        .expect("run");
    assert_eq!(gone.status.code(), Some(1));
    let gone: Value = serde_json::from_slice(&gone.stderr).expect("json");
    assert_eq!(gone["error"]["code"], "server_not_running");

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
/// A manifest's first line `id = "x"` made the head herdr requires: a name, a version and the
/// herdr it needs.
fn headed(manifest: &str) -> String {
    match manifest.split_once('\n') {
        Some((first, rest)) if first.starts_with("id = ") => format!(
            "{first}\nname = \"test\"\nversion = \"0.1.0\"\nmin_herdr_version = \"0.9.0\"\n{rest}"
        ),
        _ => manifest.to_string(),
    }
}

fn plant(data: &Path, id: &str, manifest: &str, agreed: &str, ledger: &mut Value) {
    let (manifest, agreed) = (headed(manifest), headed(agreed));
    let folder = data.join("folders").join(id);
    std::fs::create_dir_all(&folder).expect("mkdir");
    std::fs::write(folder.join("herdr-plugin.toml"), &manifest).expect("manifest");
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
        "HERDR_PLUGIN_EVENT=pane.created".to_string(),
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

fn ok(answer: &Value) -> &Value {
    if answer.get("error").is_some() || answer.is_null() {
        eprintln!("answer: {answer}");
    }
    assert!(answer.get("error").is_none(), "refused: {answer}");
    &answer["result"]
}

fn panes(stack: &Stack) -> Vec<Value> {
    stack.ask("pane.list", json!({}))["result"]["panes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn screen_has(stack: &Stack, pane: &str, text: &str) -> bool {
    stack.ask("pane.read", json!({"pane_id": pane, "source": "visible"}))["result"]["read"]["text"]
        .as_str()
        .is_some_and(|t| t.contains(text))
}

#[test]
fn writes_change_qoop_as_herdr_asks() {
    let stack = Stack::start_with(|dir| {
        let base = "id = \"t.base\"\n";
        plant(
            &dir.join("data"),
            "t.base",
            base,
            base,
            &mut json!({"plugins": {}}),
        );
        // No desktop note reaches the person running the tests.
        let config = dir.join("config/qoop");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::write(config.join("config.toml"), "[notify]\nmute = \"here\"\n").expect("config");
    });
    let first = panes(&stack)[0]["pane_id"]
        .as_str()
        .expect("id")
        .to_string();

    let split = stack.ask(
        "pane.split",
        json!({"target_pane_id": first, "direction": "right", "focus": false}),
    );
    let second = ok(&split)["pane"]["pane_id"]
        .as_str()
        .expect("id")
        .to_string();
    let now = panes(&stack);
    assert_eq!(now.len(), 2);
    assert_eq!(now[0]["tab_id"], now[1]["tab_id"], "one tab, split");

    let renamed = stack.ask("pane.rename", json!({"pane_id": second, "label": "logs"}));
    assert_eq!(ok(&renamed)["pane"]["label"], "logs");

    ok(&stack.ask(
        "pane.send_text",
        json!({"pane_id": second, "text": "echo hi-there"}),
    ));
    ok(&stack.ask(
        "pane.send_input",
        json!({"pane_id": second, "keys": ["Enter"]}),
    ));
    wait_for("the typed command's output", || {
        stack.ask("pane.read", json!({"pane_id": second, "source": "visible"}))["result"]["read"]
            ["text"]
            .as_str()
            .is_some_and(|t| t.matches("hi-there").count() >= 2)
    });
    ok(&stack.ask(
        "agent.prompt",
        json!({"target": first, "text": "echo prompted-ok"}),
    ));
    wait_for("the prompt's output", || {
        screen_has(&stack, &first, "prompted-ok\n")
    });

    let unnamed = stack.ask("pane.zoom", json!({"mode": "on"}));
    assert_eq!(
        unnamed["error"]["code"], "pane_not_found",
        "no pane named, no caller, no focus reported: refused, not guessed"
    );
    // A focus is shown in every interface, so from then on it is what is shown.
    ok(&stack.ask("pane.focus", json!({"pane_id": second})));
    assert_eq!(
        ok(&stack.ask("pane.current", json!({})))["pane"]["pane_id"],
        second.as_str()
    );
    let zoom = stack.ask("pane.zoom", json!({"pane_id": second, "mode": "on"}));
    assert_eq!(ok(&zoom)["zoom"]["zoomed"], true);
    assert_eq!(zoom["result"]["zoom"]["pane_id"], second.as_str());

    let tab = stack.ask("tab.create", json!({"label": "Files", "focus": false}));
    let tab_id = ok(&tab)["tab"]["tab_id"].as_str().expect("tab").to_string();
    assert_eq!(tab["result"]["tab"]["label"], "Files");
    let tab_renamed = stack.ask("tab.rename", json!({"tab_id": tab_id, "label": "Docs"}));
    assert_eq!(ok(&tab_renamed)["tab"]["label"], "Docs");
    ok(&stack.ask("tab.focus", json!({"tab_id": tab_id})));
    ok(&stack.ask("tab.close", json!({"tab_id": tab_id})));
    assert!(panes(&stack).iter().all(|p| p["tab_id"] != tab_id.as_str()));

    let made = stack.ask("workspace.create", json!({"label": "api", "focus": false}));
    let space = ok(&made)["workspace"]["workspace_id"]
        .as_str()
        .expect("id")
        .to_string();
    assert!(made["result"]["root_pane"]["pane_id"].is_string());
    let renamed = stack.ask(
        "workspace.rename",
        json!({"workspace_id": space, "label": "api2"}),
    );
    assert_eq!(ok(&renamed)["workspace"]["label"], "api2");
    ok(&stack.ask("workspace.focus", json!({"workspace_id": space})));
    ok(&stack.ask("workspace.close", json!({"workspace_id": space})));
    let spaces = stack.ask("workspace.list", json!({}));
    assert!(
        ok(&spaces)["workspaces"]
            .as_array()
            .expect("list")
            .iter()
            .all(|w| w["workspace_id"] != space.as_str())
    );

    ok(&stack.ask("pane.close", json!({"pane_id": second})));
    assert_eq!(panes(&stack).len(), 1);

    // A shell asked for with an environment starts with it, and it is never typed.
    let with_env = stack.ask(
        "pane.split",
        json!({"target_pane_id": first, "focus": false, "env": {"HELLO_ENV": "yes"}}),
    );
    let shell = ok(&with_env)["pane"]["pane_id"]
        .as_str()
        .expect("id")
        .to_string();
    wait_for("the new shell", || screen_has(&stack, &shell, "$"));
    ok(&stack.ask(
        "pane.send_input",
        json!({"pane_id": shell, "text": "echo $HELLO_ENV-seen", "keys": ["Enter"]}),
    ));
    wait_for("the variable in the shell", || {
        screen_has(&stack, &shell, "yes-seen")
    });
    assert!(
        !screen_has(&stack, &shell, "HELLO_ENV=yes"),
        "never on the screen"
    );
    ok(&stack.ask("pane.close", json!({"pane_id": shell})));

    let note = stack.ask(
        "notification.show",
        json!({"title": "done", "body": "green"}),
    );
    assert_eq!(ok(&note)["shown"], false, "muted here");
    assert_eq!(note["result"]["reason"], "disabled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_at_the_tab_refuses_a_plugins_write() {
    let stack = Stack::start();
    let first = panes(&stack)[0]["pane_id"]
        .as_str()
        .expect("id")
        .to_string();
    let terminal: u32 = first
        .rsplit(":p")
        .next()
        .expect("p")
        .parse()
        .expect("number");
    let socket = qoop_testkit::socket_in(stack.dir.path());
    let (stream, inbuf) = qoop_runtime::client::connect(&socket, qoop_proto::INTERFACE_ID)
        .await
        .expect("connect");
    let mut person = qoop_testkit::TestClient::from_parts(stream, inbuf, terminal);
    person.input_to(terminal, b"half a thought").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let refused = stack.ask(
        "pane.send_text",
        json!({"pane_id": first, "text": "echo no"}),
    );
    assert_eq!(refused["error"]["code"], "input_refused", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("a person is at terminal")),
        "{refused}"
    );
}

#[test]
fn a_focus_is_told_as_herdr_tells_it_and_a_status_already_there_at_once() {
    let stack = Stack::start();
    let first = panes(&stack)[0].clone();
    let pane = first["pane_id"].as_str().expect("id").to_string();
    let mut stream = UnixStream::connect(stack.herdr_socket()).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    let subscribe = json!({"id": "s", "method": "events.subscribe", "params": {"subscriptions": [
        {"type": "workspace.focused"}, {"type": "tab.focused"}, {"type": "pane.focused"},
        {"type": "pane.agent_status_changed", "pane_id": pane, "agent_status": "unknown"}
    ]}});
    stream
        .write_all(format!("{subscribe}\n").as_bytes())
        .expect("write");
    let mut lines = BufReader::new(stream).lines();
    let mut next = || -> Value {
        serde_json::from_str(&lines.next().expect("a line").expect("read")).expect("json")
    };
    assert_eq!(next()["result"]["type"], "subscription_started");
    // A shell's status is already `unknown`: told at once.
    let already = next();
    assert_eq!(already["event"], "pane.agent_status_changed", "{already}");
    assert_eq!(already["data"]["agent_status"], "unknown");

    let terminal: u64 = first["terminal_id"]
        .as_str()
        .expect("t")
        .parse()
        .expect("n");
    ok(&stack.ask("qoop.focus", json!({"terminal": terminal})));
    let told: Vec<Value> = (0..3).map(|_| next()).collect();
    let names: Vec<&str> = told
        .iter()
        .map(|e| e["event"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, ["workspace_focused", "tab_focused", "pane_focused"]);
    assert_eq!(told[2]["data"]["pane_id"], pane.as_str());
    assert_eq!(told[2]["data"]["type"], "pane_focused");
}

#[test]
fn the_interfaces_focus_is_checked_and_served() {
    let stack = Stack::start();
    let refused = stack.ask("qoop.focus", json!({"terminal": 999}));
    assert_eq!(refused["error"]["code"], "pane_not_found");
    let first = panes(&stack)[0].clone();
    let terminal: u64 = first["terminal_id"]
        .as_str()
        .expect("t")
        .parse()
        .expect("n");
    ok(&stack.ask("qoop.focus", json!({"terminal": terminal})));
    assert_eq!(panes(&stack)[0]["focused"], true);
    assert_eq!(
        ok(&stack.ask("pane.current", json!({})))["pane"]["pane_id"],
        first["pane_id"]
    );
    let snapshot = stack.ask("session.snapshot", json!({}));
    assert_eq!(
        ok(&snapshot)["snapshot"]["focused_pane_id"],
        first["pane_id"]
    );
    let unknown = stack.ask("qoop.nothing", json!({}));
    assert_eq!(unknown["error"]["code"], "invalid_request");
}

#[test]
fn a_plugin_pane_runs_its_spec_and_a_popup_hands_focus_back() {
    let manifest = "id = \"t.pane\"\n\
                    [[panes]]\nid = \"view\"\ntitle = \"Viewer\"\nplacement = \"popup\"\n\
                    command = [\"sh\", \"-c\", \"env > \\\"$HERDR_PLUGIN_STATE_DIR/pane-env.txt\\\"; sleep 30\"]\n\
                    [[actions]]\nid = \"go\"\ntitle = \"Go\"\n\
                    command = [\"sh\", \"-c\", \"env > \\\"$HERDR_PLUGIN_STATE_DIR/action-env.txt\\\"\"]\n";
    let stack = Stack::start_with(|dir| {
        plant(
            &dir.join("data"),
            "t.pane",
            manifest,
            manifest,
            &mut json!({"plugins": {}}),
        );
    });
    let first = panes(&stack)[0].clone();
    let terminal: u64 = first["terminal_id"]
        .as_str()
        .expect("t")
        .parse()
        .expect("n");
    ok(&stack.ask("qoop.focus", json!({"terminal": terminal})));

    let opened = stack.ask(
        "plugin.pane.open",
        json!({"plugin_id": "t.pane", "entrypoint": "view",
               "env": {"EXTRA": "yes", "HERDR_PLUGIN_ID": "someone.else"}}),
    );
    assert_eq!(
        ok(&opened),
        &json!({"type": "ok"}),
        "a popup answers ok, as in herdr"
    );
    let popup = panes(&stack)
        .into_iter()
        .find(|p| p["label"] == "Viewer")
        .expect("the popup, named after its title")["pane_id"]
        .as_str()
        .expect("pane")
        .to_string();
    let again = stack.ask(
        "plugin.pane.open",
        json!({"plugin_id": "t.pane", "entrypoint": "view"}),
    );
    assert_eq!(again["error"]["code"], "ui_busy", "one popup at a time");
    let state = stack.dir.path().join("data/qoop/plugins/.state/t.pane");
    let env_file = state.join("pane-env.txt");
    wait_for("the pane's program", || {
        std::fs::read_to_string(&env_file).is_ok_and(|e| e.contains("HERDR_PANE_ID="))
    });
    let env = std::fs::read_to_string(&env_file).expect("env");
    for want in [
        format!("HERDR_PANE_ID={popup}"),
        "EXTRA=yes".to_string(),
        "HERDR_WORKSPACE_ID=w1".to_string(),
        "HERDR_TAB_ID=w1:t".to_string(),
        "HERDR_PLUGIN_ENTRYPOINT_ID=view".to_string(),
        "HERDR_PLUGIN_ID=t.pane".to_string(),
    ] {
        assert!(env.contains(&want), "{want} missing from\n{env}");
    }
    assert!(!env.lines().any(|l| l.starts_with("QOOP_")), "{env}");
    assert!(
        !screen_has(&stack, &popup, "EXTRA"),
        "the spec never passes through the line editor"
    );

    assert_eq!(
        ok(&stack.ask("pane.current", json!({})))["pane"]["pane_id"],
        popup.as_str()
    );
    // A call with no pane named is about the pane before the popup.
    let zoom = stack.ask("pane.zoom", json!({"mode": "off"}));
    assert_eq!(ok(&zoom)["zoom"]["pane_id"], first["pane_id"]);
    ok(&stack.ask(
        "qoop.focus",
        json!({"terminal": popup.rsplit(":p").next().expect("p").parse::<u64>().expect("n")}),
    ));

    ok(&stack.ask("popup.close", json!({})));
    assert!(panes(&stack).iter().all(|p| p["pane_id"] != popup.as_str()));
    assert_eq!(
        ok(&stack.ask("pane.current", json!({})))["pane"]["pane_id"],
        first["pane_id"],
        "focus went back"
    );
    let none = stack.ask("popup.close", json!({}));
    assert_eq!(none["error"]["code"], "popup_not_open", "{none}");
    assert_eq!(none["error"]["message"], "no popup is open");

    // `qoop plugin run` goes through the host, with the plugin's environment.
    let ran = qoop(stack.dir.path())
        .args(["plugin", "run", "t.pane.go"])
        .output()
        .expect("run");
    assert!(ran.status.success(), "{ran:?}");
    let action_env = state.join("action-env.txt");
    wait_for("the action", || {
        std::fs::read_to_string(&action_env).is_ok_and(|e| e.contains("HERDR_PLUGIN_ACTION_ID="))
    });
    let env = std::fs::read_to_string(&action_env).expect("env");
    assert!(env.contains("HERDR_PLUGIN_ACTION_ID=go"), "{env}");
    assert!(env.contains(&format!(
        "HERDR_PANE_ID={}",
        first["pane_id"].as_str().expect("id")
    )));
}

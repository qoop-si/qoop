//! The chat of an agent terminal through the real socket: `Messages`, `MessageDetail`,
//! `MessagesChanged` and `qoop messages`.
//!
//! A real daemon runs in a sandbox home. The agent is a stand in named `claude` that tells
//! the daemon its conversation the way the real hook does (`qoop hook-report --session-id`),
//! and the conversation is a transcript written under the sandbox home where Claude keeps it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qoop_runtime::client;
use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage};
use qoop_testkit::TestClient;
use serde_json::{Value, json};

const QOOP: &str = env!("CARGO_BIN_EXE_qoop");
const PATIENCE: Duration = Duration::from_secs(20);

/// Fields a transcript carries that no client may ever be sent.
const CWD_MARKER: &str = "cwd-only-marker";
const SECRETS: [&str; 2] = ["sk-ant-api03-TOPSECRET", "abc123tok"];

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let child = qoop_testkit::sandboxed(QOOP)
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        Self { child, dir }
    }

    fn qoop(&self, args: &[&str]) -> (bool, String, String) {
        let out = qoop_testkit::sandboxed(QOOP)
            .args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .output()
            .expect("run qoop");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    async fn client(&self) -> TestClient {
        let socket = qoop_testkit::socket_in(self.dir.path());
        let (stream, inbuf) = client::connect(&socket, "messages-test")
            .await
            .expect("connect");
        TestClient::from_parts(stream, inbuf, 1)
    }

    /// Run a program named `claude` in terminal 1 that reports `session` (when it has one)
    /// and then waits, and return once the daemon lists that conversation for the terminal.
    fn start_agent(&self, session: Option<&str>) {
        self.start_agent_with(session, true);
    }

    fn start_agent_with(&self, session: Option<&str>, listed: bool) {
        let bin = self.dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let source = bin.join("claude.rs");
        let report = session.map_or(String::new(), |id| {
            format!("let _ = std::process::Command::new({QOOP:?}).args([\"hook-report\", \"--session-id\", {id:?}]).status();")
        });
        std::fs::write(
            &source,
            format!("fn main() {{ {report} std::thread::sleep(std::time::Duration::from_secs(120)); }}\n"),
        )
        .expect("write");
        let built = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(bin.join("claude"))
            .output()
            .expect("run rustc");
        assert!(
            built.status.success(),
            "the stand in did not build: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let (ok, _, err) = self.qoop(&[
            "send",
            "1",
            &bin.join("claude").display().to_string(),
            "--enter",
        ]);
        assert!(ok, "{err}");
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let row = self.row();
            let agent = row["agent"] == json!(true);
            let said = !listed || session.is_none_or(|id| row["agent_session"] == json!(id));
            if agent && said {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the stand in never showed as an agent: {}", self.row());
    }

    /// The same, for a conversation id the daemon may well refuse to keep: waits for the
    /// agent alone, and a moment for its hook to have been heard.
    fn start_agent_unchecked(&self, session: &str) {
        self.start_agent_with(Some(session), false);
        std::thread::sleep(Duration::from_millis(1500));
    }

    fn row(&self) -> Value {
        let (_, out, _) = self.qoop(&["agents", "--json"]);
        let rows: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        rows.as_array()
            .and_then(|rows| rows.iter().find(|r| r["terminal"] == json!(1)))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoop daemon`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn sandbox_home() -> PathBuf {
    qoop_testkit::sandboxed("true")
        .get_envs()
        .find(|(k, _)| *k == "HOME")
        .and_then(|(_, v)| v.map(PathBuf::from))
        .expect("the sandbox has a home")
}

/// Where Claude writes conversation `id`, under the sandbox home.
fn transcript(id: &str) -> PathBuf {
    let dir = sandbox_home().join(".claude/projects/-work-demo");
    std::fs::create_dir_all(&dir).expect("project dir");
    dir.join(format!("{id}.jsonl"))
}

fn fresh_id(name: &str) -> String {
    format!("{name}-{}-5a1c0de0", std::process::id())
}

fn record(kind: &str, n: usize, session: &str, message: Value) -> String {
    format!(
        "{}\n",
        json!({
            "type": kind, "uuid": format!("u{n:05}"), "isSidechain": false, "sessionId": session,
            "timestamp": format!("2026-10-12T09:{:02}:{:02}.000Z", n / 60 % 60, n % 60),
            "userType": "external", "entrypoint": "cli", "cwd": format!("/{CWD_MARKER}/proj"),
            "version": "2.1.296", "gitBranch": "gitbranch-only-marker", "message": message,
        })
    )
}

fn user(n: usize, session: &str, text: &str) -> String {
    record("user", n, session, json!({"role": "user", "content": text}))
}

fn agent(n: usize, session: &str, text: &str) -> String {
    record(
        "assistant",
        n,
        session,
        json!({"id": format!("msg_{n}"), "role": "assistant", "model": "claude-sonnet-5-5",
               "content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
               "usage": {"input_tokens": 1, "output_tokens": 1}}),
    )
}

fn call(n: usize, session: &str, id: &str, command: &str) -> String {
    record(
        "assistant",
        n,
        session,
        json!({"id": format!("msg_{n}"), "role": "assistant", "model": "claude-sonnet-5-5",
               "content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}],
               "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 1}}),
    )
}

fn result(n: usize, session: &str, id: &str, text: &str) -> String {
    record(
        "user",
        n,
        session,
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id,
                                            "content": text, "is_error": false}]}),
    )
}

/// A time after any agent this test starts.
const LATER: &str = "2099-01-01T00:00:00.000Z";

/// The record `line` with a later time.
fn later(line: String) -> String {
    let mut record: Value = serde_json::from_str(&line).expect("a record");
    record["timestamp"] = json!(LATER);
    format!("{record}\n")
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open")
        .write_all(text.as_bytes())
        .expect("append");
}

fn turns(session: &str, count: usize) -> String {
    (0..count)
        .flat_map(|n| {
            [
                user(2 * n, session, &format!("question {n}")),
                agent(2 * n + 1, session, &format!("answer {n}")),
            ]
        })
        .collect()
}

async fn ask(client: &mut TestClient, request: ControlRequest) -> ControlResponse {
    client
        .control(&request, PATIENCE)
        .await
        .expect("the daemon answered")
}

fn messages(skip: u32, limit: u32, revision: Option<u64>) -> ControlRequest {
    ControlRequest::Messages {
        id: format!("m-{skip}-{limit}-{revision:?}"),
        terminal: 1,
        skip,
        limit,
        revision,
    }
}

struct Page {
    revision: u64,
    reset: bool,
    has_older: bool,
    items: Vec<Value>,
}

fn page(response: ControlResponse) -> Page {
    let ControlResponse::Messages {
        revision,
        reset,
        has_older,
        items,
        ..
    } = response
    else {
        panic!("a page was expected: {response:?}");
    };
    Page {
        revision,
        reset,
        has_older,
        items: items
            .iter()
            .map(|i| serde_json::to_value(i).expect("item"))
            .collect(),
    }
}

fn texts(page: &Page) -> Vec<String> {
    page.items
        .iter()
        .filter_map(|i| i["text"].as_str().map(str::to_string))
        .collect()
}

fn refusal(response: ControlResponse) -> String {
    match response {
        ControlResponse::Failed { reason, .. } => reason,
        other => panic!("a refusal was expected: {other:?}"),
    }
}

#[tokio::test]
async fn a_shell_is_not_an_agent_and_an_agent_without_a_session_has_none() {
    let daemon = Daemon::start();
    let mut c = daemon.client().await;
    assert_eq!(
        refusal(ask(&mut c, messages(0, 10, None)).await),
        "not_agent"
    );
    assert_eq!(
        refusal(
            ask(
                &mut c,
                ControlRequest::MessageDetail {
                    id: "d".into(),
                    terminal: 1,
                    tool_id: "t".into(),
                    max_bytes: None,
                }
            )
            .await
        ),
        "not_agent"
    );
    let (ok, _, err) = daemon.qoop(&["messages", "1"]);
    assert!(!ok && err.contains("not_agent"), "{err}");
    assert_eq!(
        refusal(
            ask(
                &mut c,
                ControlRequest::Messages {
                    id: "far".into(),
                    terminal: 99,
                    skip: 0,
                    limit: 1,
                    revision: None
                }
            )
            .await
        ),
        "unavailable"
    );

    // A claude that never said which conversation it is in.
    daemon.start_agent(None);
    assert_eq!(
        refusal(ask(&mut c, messages(0, 10, None)).await),
        "no_session"
    );
}

#[tokio::test]
async fn a_session_that_is_gone_has_no_session() {
    let daemon = Daemon::start();
    let id = fresh_id("gone");
    let file = transcript(&id);
    std::fs::write(&file, turns(&id, 2)).expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;
    assert_eq!(
        page(ask(&mut c, messages(0, 10, None)).await).items.len(),
        4
    );
    std::fs::remove_file(&file).expect("remove");
    assert_eq!(
        refusal(ask(&mut c, messages(0, 10, None)).await),
        "no_session"
    );
}

#[tokio::test]
async fn the_chat_pages_back_and_starts_over_when_the_file_changed() {
    let daemon = Daemon::start();
    let id = fresh_id("pages");
    let file = transcript(&id);
    std::fs::write(&file, turns(&id, 15)).expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;

    // The newest ten of thirty items, oldest first.
    let first = page(ask(&mut c, messages(0, 10, None)).await);
    assert!(!first.reset && first.has_older);
    let newest = texts(&first);
    assert_eq!(newest.len(), 10);
    assert_eq!(newest.last().unwrap(), "answer 14");
    // Nothing of the transcript but what a chat shows: no cwd, no branch, no raw record.
    let wire = serde_json::to_string(&first.items).unwrap();
    assert!(
        !wire.contains(CWD_MARKER) && !wire.contains("gitbranch-only-marker"),
        "{wire}"
    );

    // The next ten older, at the same revision: the ten before, none of them twice.
    let older = page(ask(&mut c, messages(10, 10, Some(first.revision))).await);
    assert!(!older.reset && older.revision == first.revision);
    let before = texts(&older);
    assert_eq!(before.len(), 10);
    assert_eq!(before.last().unwrap(), "answer 9");
    assert!(before.iter().all(|t| !newest.contains(t)));

    // The file grows: the old revision no longer holds, `skip` is ignored and the newest
    // page comes back, saying so.
    append(&file, &user(100, &id, "a late question"));
    let again = page(ask(&mut c, messages(10, 10, Some(first.revision))).await);
    assert!(again.reset && again.revision != first.revision);
    assert_eq!(texts(&again).last().unwrap(), "a late question");
    assert_eq!(again.items.len(), 10);
    // The same question at the current revision pages as before.
    let paged = page(ask(&mut c, messages(10, 10, Some(again.revision))).await);
    assert!(!paged.reset);
    assert_eq!(texts(&paged).last().unwrap(), "question 10");

    // The CLI: one row a line, newest last, and the page as JSON.
    let (ok, out, err) = daemon.qoop(&["messages", "1", "--limit", "3"]);
    assert!(ok, "{err}");
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), 3, "{out}");
    assert!(
        rows[2].contains("you") && rows[2].contains("a late question"),
        "{out}"
    );
    let (ok, out, _) = daemon.qoop(&["messages", "1", "--skip", "1", "--limit", "2", "--json"]);
    assert!(ok);
    let json: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(json["items"].as_array().unwrap().len(), 2);
    assert_eq!(json["items"][1]["text"], "answer 14");
    assert_eq!(json["revision"], json!(again.revision));
}

/// A running subagent, input still queued, a command's coloured output and a turn that ended
/// with agents still running reach `qoop messages --json` as the fields a client draws: the
/// subagent's `status`, the user row's `queued`, plain text, and one `background` notice once
/// a turn ends with agents still running.
#[tokio::test]
async fn what_runs_in_the_background_and_what_waits_in_the_queue_is_in_the_json() {
    let daemon = Daemon::start();
    let id = fresh_id("parity");
    let file = transcript(&id);
    let escaped = "\u{1b}[2mCompacted (ctrl+o to see full summary)\u{1b}[22m";
    let lines = [
        user(0, &id, "start the survey"),
        user(
            1,
            &id,
            &format!("<local-command-stdout>{escaped}</local-command-stdout>"),
        ),
        format!(
            "{}\n",
            json!({"type": "queue-operation", "operation": "enqueue", "sessionId": id,
                   "timestamp": "2026-10-12T09:00:02.000Z", "content": "then run the lint"})
        ),
        // The call is the last message and later than the agent's own start: it still runs.
        later(record(
            "assistant",
            3,
            &id,
            json!({"id": "msg_3", "role": "assistant", "model": "claude-sonnet-5-5",
                   "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent",
                                "input": {"description": "survey the tests", "prompt": "go"}}],
                   "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 1}}),
        )),
    ];
    std::fs::write(&file, lines.concat()).expect("write");
    daemon.start_agent(Some(&id));

    let (ok, out, err) = daemon.qoop(&["messages", "1", "--json"]);
    assert!(ok, "{err}");
    let json: Value = serde_json::from_str(&out).expect("json");
    let items = json["items"].as_array().expect("items");
    let find = |kind: &str| items.iter().find(|i| i["type"] == kind).cloned();
    let subagent = find("subagent").expect("the subagent row");
    assert_eq!(subagent["status"], "running", "{subagent}");
    let queued = items
        .iter()
        .find(|i| i["text"] == "then run the lint")
        .expect("the queued prompt");
    assert_eq!(queued["type"], "user");
    assert_eq!(queued["queued"], true, "{queued}");
    let first = items.iter().find(|i| i["text"] == "start the survey");
    assert!(
        first.is_some_and(|i| i.get("queued").is_none()),
        "false is not sent"
    );
    let output = items
        .iter()
        .find(|i| i["kind"] == "command output")
        .expect("the command output");
    assert_eq!(output["text"], "Compacted (ctrl+o to see full summary)");
    assert!(
        !out.contains("\\u001b"),
        "no escape reaches a client: {out}"
    );
    assert!(
        items.iter().all(|i| i["kind"] != "background"),
        "no turn has ended yet"
    );
    // The text form says the same in words.
    let (ok, text, _) = daemon.qoop(&["messages", "1"]);
    assert!(ok);
    assert!(
        text.contains("[running] spawned: survey the tests (0 steps)"),
        "{text}"
    );
    assert!(text.contains("[queued] then run the lint"), "{text}");
    // The turn ends with two agents still running: one quiet row says so.
    append(
        &file,
        &format!(
            "{}\n",
            json!({"type": "system", "subtype": "turn_duration", "uuid": "u00004", "sessionId": id,
                   "timestamp": LATER, "durationMs": 1200,
                   "pendingBackgroundAgentCount": 2, "pendingWorkflowCount": 0})
        ),
    );
    let (ok, out, err) = daemon.qoop(&["messages", "1", "--json"]);
    assert!(ok, "{err}");
    let json: Value = serde_json::from_str(&out).expect("json");
    let background = json["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["kind"] == "background"));
    assert_eq!(
        background.map(|i| i["text"].clone()),
        Some(json!("2 background agents still running")),
        "{out}"
    );
}

/// Work the transcript shows as running from before the agent in the terminal started was left
/// by an agent that is gone (the session was resumed): it reads `interrupted`, and the count of
/// agents still running of a turn from that time is not said.
#[tokio::test]
async fn work_left_running_by_an_earlier_agent_is_interrupted_not_a_spinner() {
    let daemon = Daemon::start();
    let id = fresh_id("stale");
    let file = transcript(&id);
    let lines = [
        user(0, &id, "start the survey"),
        record(
            "assistant",
            1,
            &id,
            json!({"id": "msg_1", "role": "assistant", "model": "claude-sonnet-5-5",
                   "content": [{"type": "tool_use", "id": "toolu_bg", "name": "Bash",
                                "input": {"command": "sleep 600", "run_in_background": true}}],
                   "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 1}}),
        ),
        // Its launch result: the command runs on until a notification says it ended.
        {
            let mut launched = result(
                2,
                &id,
                "toolu_bg",
                "Command running in background with ID: bx1",
            );
            let mut record: Value = serde_json::from_str(&launched).expect("a record");
            record["toolUseResult"] = json!({"backgroundTaskId": "bx1"});
            launched = format!("{record}\n");
            launched
        },
        record(
            "assistant",
            2,
            &id,
            json!({"id": "msg_2", "role": "assistant", "model": "claude-sonnet-5-5",
                   "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent",
                                "input": {"description": "survey the tests", "prompt": "go"}}],
                   "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 1}}),
        ),
        format!(
            "{}\n",
            json!({"type": "system", "subtype": "turn_duration", "uuid": "u00004", "sessionId": id,
                   "timestamp": "2026-10-12T09:00:04.000Z", "durationMs": 1200,
                   "pendingBackgroundAgentCount": 2, "pendingWorkflowCount": 0})
        ),
    ];
    std::fs::write(&file, lines.concat()).expect("write");
    daemon.start_agent(Some(&id));
    let (ok, out, err) = daemon.qoop(&["messages", "1", "--json"]);
    assert!(ok, "{err}");
    let json: Value = serde_json::from_str(&out).expect("json");
    let items = json["items"].as_array().expect("items");
    assert!(
        items.iter().all(|i| i["status"] != "running"),
        "nothing may spin: {out}"
    );
    assert!(items.iter().any(|i| i["status"] == "interrupted"), "{out}");
    assert!(items.iter().all(|i| i["kind"] != "background"), "{out}");
}

#[tokio::test]
async fn a_watched_chat_says_when_its_file_grows_and_is_quiet_when_it_does_not() {
    let daemon = Daemon::start();
    let id = fresh_id("watch");
    let file = transcript(&id);
    std::fs::write(&file, turns(&id, 2)).expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;
    let served = page(ask(&mut c, messages(0, 10, None)).await);
    let changed = |e: &EventMessage| matches!(e, EventMessage::MessagesChanged { terminal: 1, .. });

    // Nothing changed: nothing is said (well past the one second tick).
    assert!(
        c.wait_for_event(changed, Duration::from_millis(2500))
            .await
            .is_none()
    );

    append(&file, &agent(50, &id, "more words"));
    let event = c
        .wait_for_event(changed, Duration::from_secs(6))
        .await
        .expect("the daemon never said the chat changed");
    let EventMessage::MessagesChanged { revision, .. } = event else {
        unreachable!()
    };
    assert_ne!(revision, served.revision);
    // And that revision is what a read answers now.
    let now = page(ask(&mut c, messages(0, 10, Some(served.revision))).await);
    assert!(now.reset);
    assert_eq!(now.revision, revision);

    // Said once for one change, not every second.
    c.forget_events();
    assert!(
        c.wait_for_event(changed, Duration::from_millis(2500))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn a_page_keeps_to_its_budget_and_a_long_chat_still_ends_with_the_newest() {
    let daemon = Daemon::start();
    let id = fresh_id("big");
    let file = transcript(&id);
    // Four thousand characters a message, quotes and newlines in them, so the text grows
    // when it is written as JSON.
    let body = "he said \"go\"\nand went. ".repeat(180);
    let mut text = String::new();
    for n in 0..300 {
        text.push_str(&user(2 * n, &id, &format!("{n} {body}")));
        text.push_str(&agent(2 * n + 1, &id, &format!("{n} {body}")));
    }
    std::fs::write(&file, text).expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;

    let big = page(ask(&mut c, messages(0, 200, None)).await);
    let bytes = serde_json::to_vec(&big.items).unwrap().len();
    assert!(bytes <= 256 * 1024, "{bytes} bytes of items");
    assert!(
        big.items.len() < 200 && big.has_older,
        "{} items",
        big.items.len()
    );
    assert!(texts(&big).last().unwrap().starts_with("299 "));
}

#[tokio::test]
async fn a_tool_call_opens_with_its_secrets_masked_and_cut_where_asked() {
    let daemon = Daemon::start();
    let id = fresh_id("detail");
    let file = transcript(&id);
    let command = format!(
        "curl -H \"Authorization: Bearer {}\" https://api.example.com",
        SECRETS[0]
    );
    std::fs::write(
        &file,
        [
            user(0, &id, "call it"),
            call(1, &id, "toolu_curl", &command),
            result(
                2,
                &id,
                "toolu_curl",
                &format!("ok\nAPI_TOKEN={}\n{}", SECRETS[1], "x".repeat(5000)),
            ),
        ]
        .concat(),
    )
    .expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;

    // The page says a call happened and has no output in it, and no secret in its summary.
    let listed = page(ask(&mut c, messages(0, 10, None)).await);
    let tool = listed
        .items
        .iter()
        .find(|i| i["type"] == "tool")
        .expect("a tool row");
    assert_eq!(tool["id"], "toolu_curl");
    assert_eq!(tool["has_detail"], true);
    let wire = serde_json::to_string(&listed.items).unwrap();
    assert!(SECRETS.iter().all(|s| !wire.contains(s)), "{wire}");
    assert!(!wire.contains("API_TOKEN"), "{wire}");

    let detail = |max_bytes| ControlRequest::MessageDetail {
        id: format!("d-{max_bytes:?}"),
        terminal: 1,
        tool_id: "toolu_curl".into(),
        max_bytes,
    };
    let ControlResponse::MessageDetail {
        input,
        output,
        truncated,
        ..
    } = ask(&mut c, detail(None)).await
    else {
        panic!("detail expected");
    };
    assert!(input.contains("api.example.com") && output.starts_with("ok"));
    assert!(
        SECRETS
            .iter()
            .all(|s| !input.contains(s) && !output.contains(s)),
        "{input} {output}"
    );
    assert!(!truncated);

    let ControlResponse::MessageDetail {
        output, truncated, ..
    } = ask(&mut c, detail(Some(64))).await
    else {
        panic!("detail expected");
    };
    assert!(truncated && output.len() <= 64 + 16, "{}", output.len());

    // A call the file does not have.
    let missing = ControlRequest::MessageDetail {
        id: "d-missing".into(),
        terminal: 1,
        tool_id: "toolu_nope".into(),
        max_bytes: None,
    };
    assert_eq!(refusal(ask(&mut c, missing).await), "no_session");

    // The CLI form.
    let (ok, out, err) = daemon.qoop(&["messages", "detail", "1", "toolu_curl", "--json"]);
    assert!(ok, "{err}");
    let json: Value = serde_json::from_str(&out).expect("json");
    assert!(json["input"].as_str().unwrap().contains("api.example.com"));
    assert!(SECRETS.iter().all(|s| !out.contains(s)), "{out}");
}

#[tokio::test]
async fn a_huge_skip_is_an_empty_page_and_a_hostile_hook_id_reads_nothing() {
    let daemon = Daemon::start();
    let id = fresh_id("skip");
    std::fs::write(transcript(&id), turns(&id, 20)).expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;

    // Far past the oldest item: nothing, nothing older, and no more work than a page.
    let started = Instant::now();
    let past = page(ask(&mut c, messages(u32::MAX, 60, None)).await);
    assert!(past.items.is_empty() && !past.has_older);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let last = page(ask(&mut c, messages(39, 60, None)).await);
    assert_eq!(last.items.len(), 1);
    assert!(!last.has_older);

    // A second agent whose hook says it is in a conversation named like a path out of the
    // project directory, with a transcript waiting where that path leads.
    let daemon = Daemon::start();
    let loot = sandbox_home().join(".claude/projects/loot.jsonl");
    std::fs::write(&loot, turns("loot", 3)).expect("write");
    daemon.start_agent_unchecked("../loot");
    let mut c = daemon.client().await;
    let refused = refusal(ask(&mut c, messages(0, 10, None)).await);
    assert_eq!(refused, "no_session");
    let (ok, out, _) = daemon.qoop(&["messages", "1", "--json"]);
    assert!(!ok && !out.contains("question 0"), "{out}");
}

/// Forty clients ask at once about one terminal. The daemon answers every one: a page, or
/// `unavailable` at once when the terminal already has its few reads in flight or the two chat
/// threads are taken. Never a hang, never an error of another kind, and it still answers after.
#[tokio::test]
async fn a_flood_of_reads_is_answered_or_refused_and_the_daemon_stays_up() {
    let daemon = Daemon::start();
    let id = fresh_id("flood");
    let file = transcript(&id);
    let body = "x".repeat(1800);
    let mut text = String::new();
    for n in 0..1500 {
        text.push_str(&user(2 * n, &id, &format!("q{n} {body}")));
        text.push_str(&agent(2 * n + 1, &id, &format!("a{n} {body}")));
    }
    std::fs::write(&file, text).expect("write");
    daemon.start_agent(Some(&id));

    let mut reads = tokio::task::JoinSet::new();
    for n in 0..40u32 {
        let mut c = daemon.client().await;
        reads.spawn(async move {
            // Different pages, so the memo cannot answer them all.
            let request = ControlRequest::Messages {
                id: format!("flood-{n}"),
                terminal: 1,
                skip: 40 * n,
                limit: 60,
                revision: None,
            };
            c.control(&request, Duration::from_secs(30)).await
        });
    }
    let (mut pages, mut refused) = (0, 0);
    while let Some(answer) = reads.join_next().await {
        match answer.expect("task").expect("every read is answered") {
            ControlResponse::Messages { .. } => pages += 1,
            ControlResponse::Failed { reason, .. } if reason == "unavailable" => refused += 1,
            other => panic!("neither a page nor a refusal: {other:?}"),
        }
    }
    assert_eq!(pages + refused, 40);
    assert!(pages >= 1, "nothing was answered");
    let mut c = daemon.client().await;
    assert!(
        !page(ask(&mut c, messages(0, 5, None)).await)
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn a_watch_begun_by_a_detail_read_or_a_missing_file_says_nothing_until_something_happens() {
    let daemon = Daemon::start();
    let id = fresh_id("begun");
    let file = transcript(&id);
    std::fs::write(
        &file,
        [
            user(0, &id, "go"),
            call(1, &id, "toolu_a", "ls"),
            result(2, &id, "toolu_a", "ok"),
        ]
        .concat(),
    )
    .expect("write");
    daemon.start_agent(Some(&id));
    let mut c = daemon.client().await;
    let changed = |e: &EventMessage| matches!(e, EventMessage::MessagesChanged { terminal: 1, .. });

    // A watch that began with a detail read starts from the file as it is: no news at once.
    let detail = ControlRequest::MessageDetail {
        id: "d".into(),
        terminal: 1,
        tool_id: "toolu_a".into(),
        max_bytes: None,
    };
    assert!(matches!(
        ask(&mut c, detail).await,
        ControlResponse::MessageDetail { .. }
    ));
    assert!(
        c.wait_for_event(changed, Duration::from_millis(2500))
            .await
            .is_none()
    );

    // The file goes, the read is `no_session`, and the watch that begins with that says
    // nothing while it is gone (looked for every five seconds, not every tick) ...
    std::fs::remove_file(&file).expect("remove");
    assert_eq!(
        refusal(ask(&mut c, messages(0, 10, None)).await),
        "no_session"
    );
    assert!(
        c.wait_for_event(changed, Duration::from_millis(2500))
            .await
            .is_none()
    );
    // ... and says so once when it is back.
    std::fs::write(&file, turns(&id, 3)).expect("write");
    let event = c
        .wait_for_event(changed, Duration::from_secs(12))
        .await
        .expect("the file came back and nobody was told");
    let EventMessage::MessagesChanged { revision, .. } = event else {
        unreachable!()
    };
    c.forget_events();
    assert!(
        c.wait_for_event(changed, Duration::from_millis(2500))
            .await
            .is_none()
    );
    let now = page(ask(&mut c, messages(0, 10, None)).await);
    assert_eq!(now.revision, revision);
}

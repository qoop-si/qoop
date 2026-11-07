//! `qoop ask` against a live daemon, through the real socket, with stand-in agents named
//! `claude` (a shell script on the PATH): who may write a card, where an answer goes (the
//! worker itself, or qass), and that an answer is never refused: it is kept and typed once its
//! target is at rest (and, for qass, nobody is typing in it).
//!
//! The cards are written from a shell inside qass's space, as `escalate.sh` runs them, so the
//! token is the real one the daemon gave that terminal.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// A stand-in agent: reports idle (with a session when `STUB_SESSION` is set), logs every
/// line it is sent, and flips its state on `BUSY` and `FREE`. `ASK` shows a question dialog
/// as the claude hook reports one, `MENU` a permission menu, `TOOL` a tool, `TASK` opens
/// a task, `WAIT` sets its stage to `waiting-human`, `DONE <words>` ends a turn on those last
/// words, `SAID <words>` is his prompt and `EXIT` ends the agent, leaving its shell.
const STUB: &str = r#"#!/bin/sh
if [ -n "$STUB_SESSION" ]; then qoop hook-report --state idle --session-id "$STUB_SESSION"; else qoop hook-report --state idle; fi
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$STUB_LOG"
  case "$line" in
    BUSY) qoop hook-report --state working ;;
    FREE) qoop hook-report --state idle ;;
    RELAY*) qoop hook-report --state idle --event user --label "The owner answered ${line#RELAY }: in the chat" ;;
    TASK) qoop task new "ship the login fix" ;;
    ASK) qoop hook-report --state blocked --visible-blocker --event blocked --label elicitation_dialog ;;
    MENU) qoop hook-report --state blocked --visible-blocker --event blocked --label "Bash rm -rf build" ;;
    TOOL) qoop hook-report --state working --visible-working --event tool --label "Read src/main.rs" ;;
    DONE*) qoop hook-report --state idle --visible-idle --event done --label "${line#DONE }" ;;
    SAID*) qoop hook-report --state working --event user --label "${line#SAID }" ;;
    WAIT) qoop task stage waiting-human ;;
    EDIT) qoop task begin ;;
    EXIT) exit 0 ;;
  esac
done
"#;

const CARD: &str = "asked: drop users.legacy_id
doing: wrote migration 0042, dry run on the copy is clean
why: the PR cannot open before 0042 has run once on staging
asks: Run 0042 on staging now?
A: run now | no rollback exists, staging is a copy | Run 0042 on staging now, then report the row counts.
B: wait for you | you may want to read the SQL first | Do not run 0042 yet; stay idle and wait for the owner.
pick: B | a column drop cannot be undone
";

struct Stack {
    child: Child,
    dir: tempfile::TempDir,
}

impl Stack {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qoop"), bin.join("qoop")).unwrap();
        std::fs::write(bin.join("claude"), STUB).unwrap();
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            // Its own data folder: the watching switch is a file in it, and the tests that turn it
            // would otherwise turn it for each other.
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("SHELL", "/bin/sh")
            // A stand-in woken from sleep is started bare by the daemon, with no `STUB_LOG=` of
            // its own: it logs here, the file `qass` is read from.
            .env("STUB_LOG", dir.path().join("qass.log"))
            // A question dialog gets its draft after 2 seconds, not 120.
            .env("QOOP_DIALOG_DRAFT_SECS", "2")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for var in ["QOOP_TERMINAL", "QOOP_TOKEN", "QORO_TERMINAL", "QORO_TOKEN"] {
            cmd.env_remove(var);
        }
        let child = cmd.spawn().expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        let stack = Self { child, dir };
        std::fs::write(stack.path("card.txt"), CARD).unwrap();
        stack
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `qoop` from outside every terminal of this daemon.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"));
        for var in ["QOOP_TERMINAL", "QOOP_TOKEN", "QORO_TERMINAL", "QORO_TOKEN"] {
            cmd.env_remove(var);
        }
        let out = cmd.output().expect("run qoop");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `qoop` from outside every terminal, with `input` on its standard input.
    fn run_stdin(&self, args: &[&str], input: &str) -> (bool, String, String) {
        use std::io::Write;
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in ["QOOP_TERMINAL", "QOOP_TOKEN", "QORO_TERMINAL", "QORO_TOKEN"] {
            cmd.env_remove(var);
        }
        let mut child = cmd.spawn().expect("run qoop");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("qoop output");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// A new terminal in `space`, named `name`, typing `command` when given. Its number.
    fn open(&self, space: &str, name: &str, command: &[&str]) -> String {
        let mut args = vec!["new", "--name", name, "--space", space];
        if !command.is_empty() {
            args.push("--");
            args.extend(command);
        }
        let (ok, out, err) = self.run(&args);
        assert!(ok, "new failed: {err}");
        out.trim().to_string()
    }

    /// A stand-in agent in `space`, with a session id when `session` is given.
    fn agent(&self, space: &str, name: &str, session: Option<&str>) -> String {
        let log = format!("STUB_LOG={}", self.path(&format!("{name}.log")).display());
        let session = session.map(|s| format!("STUB_SESSION={s}"));
        let mut command = vec![log.as_str()];
        command.extend(session.as_deref());
        command.push("claude");
        let terminal = self.open(space, name, &command);
        assert!(
            self.wait(|| self.state_of(&terminal) == "idle"),
            "{name} never read idle: {:?}",
            self.run(&["agents"]).1
        );
        terminal
    }

    /// A shell tab in qass's folder (as the terminals see it: under the daemon's own home),
    /// opened from `from`'s shell. Its number.
    fn qass_tab(&self, from: &str) -> String {
        self.inside(
            from,
            "d=\"$HOME/Library/Application Support/qoop/qass\"; [ \"$(uname)\" = Darwin ] || d=\"$HOME/.local/share/qoop/qass\"; [ -z \"$XDG_DATA_HOME\" ] || d=\"$XDG_DATA_HOME/qoop/qass\"; mkdir -p \"$d\" && (cd \"$d\" && qoop new --space qass --name qtab)",
        );
        let (_, list, _) = self.run(&["agents"]);
        list.lines()
            .find(|l| l.contains("qtab"))
            .and_then(|l| l.strip_prefix("term="))
            .and_then(|l| l.split(' ').next())
            .unwrap_or_else(|| panic!("no qtab: {list}"))
            .to_string()
    }

    fn state_of(&self, terminal: &str) -> String {
        let (_, out, _) = self.run(&["agents"]);
        out.lines()
            .find(|l| l.starts_with(&format!("term={terminal} ")))
            .and_then(|l| l.split("state=").nth(1))
            .and_then(|s| s.split(' ').next())
            .unwrap_or("")
            .to_string()
    }

    /// The tabs `qoop ls` lists as asleep, in every space.
    fn asleep(&self) -> Vec<String> {
        let (_, out, _) = self.run(&["ls", "--json"]);
        let spaces: serde_json::Value = serde_json::from_str(&out).expect("json");
        spaces
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|s| s["sleeping"].as_array().cloned().unwrap_or_default())
            .map(|t| t.to_string())
            .collect()
    }

    fn wait(&self, mut until: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if until() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Run a command line in terminal `terminal`'s shell, as qass would, and wait for it.
    fn inside(&self, terminal: &str, line: &str) -> String {
        let out = self.path("inside.out");
        let _ = std::fs::remove_file(&out);
        let full = format!("{line} > '{0}' 2>&1; touch '{0}.done'", out.display());
        let done = self.path("inside.out.done");
        let _ = std::fs::remove_file(&done);
        let (ok, _, err) = self.run(&["send", terminal, &full, "--enter"]);
        assert!(ok, "send failed: {err}");
        assert!(self.wait(|| done.exists()), "{line} never finished");
        std::fs::read_to_string(out).unwrap_or_default()
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(&format!("{name}.log"))).unwrap_or_default()
    }

    fn card_status(&self, card: &str) -> String {
        let (_, out, _) = self.run(&["ask", "ls", "--json"]);
        let all: serde_json::Value = serde_json::from_str(&out).expect("json");
        ["live", "ended"]
            .iter()
            .flat_map(|k| all[k].as_array().cloned().unwrap_or_default())
            .find(|c| c["card"] == card)
            .map(|c| c["status"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    }

    /// The daemon's drafts about `terminal`, oldest first.
    fn drafts(&self, terminal: &str) -> Vec<serde_json::Value> {
        let (_, out, _) = self.run(&["ask", "ls", "--json"]);
        let all: serde_json::Value = serde_json::from_str(&out).expect("json");
        let mut drafts: Vec<serde_json::Value> = ["live", "ended"]
            .iter()
            .flat_map(|k| all[k].as_array().cloned().unwrap_or_default())
            .filter(|c| {
                c["terminal"].as_u64().map(|t| t.to_string()).as_deref() == Some(terminal)
                    && c["card"].as_str().is_some_and(|n| n.starts_with('W'))
            })
            .collect();
        drafts.sort_by_key(|c| c["card"].as_str().unwrap_or("")[1..].parse::<i64>().ok());
        drafts
    }

    fn send(&self, terminal: &str, line: &str) {
        let (ok, _, err) = self.run(&["send", terminal, line, "--enter"]);
        assert!(ok, "send failed: {err}");
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoop daemon`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn add(stack: &Stack, terminal: &str, card: &str, agent: &str, handle: &str) -> String {
    stack.inside(
        terminal,
        &format!(
            "qoop ask add {card} --agent {agent} --handle {handle} --urgency later < '{}'",
            stack.path("card.txt").display()
        ),
    )
}

#[test]
fn only_a_terminal_of_qass_writes_a_card_and_only_the_owner_answers_it() {
    let stack = Stack::start();
    let worker = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);

    // From outside every terminal, and from a worker's own terminal: refused.
    let (ok, _, err) = stack.run_stdin(
        &[
            "ask",
            "add",
            "E1",
            "--agent",
            &format!("T{worker}"),
            "--handle",
            "korea",
            "--urgency",
            "later",
        ],
        CARD,
    );
    assert!(
        !ok && err.contains("qass's own terminal"),
        "no card from outside a terminal: {err}"
    );
    let plain = stack.open("work", "plain", &[]);
    let said = add(&stack, &plain, "E1", &format!("T{worker}"), "korea");
    assert!(said.contains("qass's own"), "{said}");
    assert_eq!(stack.card_status("E1"), "", "nothing was stored");

    // From qass's space: stored. The same card again changes nothing.
    let said = add(&stack, &shell, "E1", &format!("T{worker}"), "korea");
    assert!(said.contains("E1 Added"), "{said}");
    let said = add(&stack, &shell, "E1", &format!("T{worker}"), "korea");
    assert!(said.contains("Unchanged"), "{said}");
    assert_eq!(stack.card_status("E1"), "open");

    // A worker cannot answer, put away or close a card from its terminal.
    for line in [
        "qoop ask answer E1 --option a",
        "qoop ask dismiss E1",
        "qoop ask close E1 --note mine",
    ] {
        let said = stack.inside(&plain, line);
        assert!(
            !said.contains("Answered") && !said.contains("Closed"),
            "{line}: {said}"
        );
    }
    assert_eq!(stack.card_status("E1"), "open");

    // A bad card is refused with every problem, and stores nothing.
    std::fs::write(stack.path("bad.txt"), "doing: x\nA: only | two\n").unwrap();
    let said = stack.inside(
        &shell,
        &format!(
            "qoop ask add E5 --agent - --handle h --urgency now < '{}'",
            stack.path("bad.txt").display()
        ),
    );
    assert!(
        said.contains("why is missing") && said.contains("card was not stored"),
        "{said}"
    );
    assert_eq!(stack.card_status("E5"), "");
}

/// The owner's watching switch turned back on: qass, at rest, is told once to start its
/// watcher, by the same flusher that types the inbox's answers. Off tells it nothing.
#[test]
fn watching_turned_on_again_is_typed_to_qass_once() {
    let stack = Stack::start();
    let _qass = stack.agent("qass", "qass", None);
    let (ok, out, err) = stack.run(&["qass", "watch", "off"]);
    assert!(ok && out.trim() == "watching off", "{out} {err}");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(stack.log("qass"), "", "off typed something");
    let (ok, out, err) = stack.run(&["qass", "watch", "on"]);
    assert!(ok && out.trim() == "watching resumed", "{out} {err}");
    // The stand-in was typed into when it opened: the inbox's 30 quiet seconds come first.
    let told = || {
        stack
            .log("qass")
            .contains("watching on: start your watcher")
    };
    assert!(
        stack.wait(told) || stack.wait(told),
        "qass was never told: {:?}",
        stack.log("qass")
    );
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stack.log("qass").matches("watching on:").count(), 1);
}

/// qass's watcher gone with its session, no switch turned (2026-10-24: 16 hours with no
/// watcher): the daemon, looking once a minute, sees no watcher twice in a row and tells qass
/// once to start it again. Slow, so run by hand: `cargo test --test ask -- --ignored gone`.
#[test]
#[ignore = "slow: the daemon looks once a minute"]
fn a_qass_whose_watcher_is_gone_is_told_to_start_it_again() {
    let stack = Stack::start();
    let _qass = stack.agent("qass", "qass", None);
    let told = || {
        stack
            .log("qass")
            .contains("watching on: start your watcher")
    };
    let deadline = Instant::now() + Duration::from_secs(200);
    while !told() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(5));
    }
    assert!(told(), "qass was never told: {:?}", stack.log("qass"));
    // Once: the next tell waits 30 minutes.
    std::thread::sleep(Duration::from_secs(70));
    assert_eq!(stack.log("qass").matches("watching on:").count(), 1);
}

/// The owner's eye opened while qass sleeps: the daemon wakes qass's own tab, and once it is
/// back and at rest the line is typed, once, with no message from the owner. Turned off, it
/// wakes nothing; another sleeping tab in qass's space stays asleep.
#[test]
fn watching_turned_on_wakes_a_sleeping_qass_and_tells_it_once() {
    let stack = Stack::start();
    let qass = stack.agent("qass", "qass", Some("sess-qass"));
    let other = stack.agent("qass", "parked", Some("sess-parked"));
    for tab in [&other, &qass] {
        assert!(
            stack.wait(|| stack.run(&["sleep", "--tab", tab]).0),
            "tab {tab} never went to sleep"
        );
    }
    let (ok, out, err) = stack.run(&["qass", "watch", "off"]);
    assert!(ok && out.trim() == "watching off", "{out} {err}");
    std::thread::sleep(Duration::from_secs(3));
    assert!(stack.asleep().contains(&qass), "off woke qass");
    let (ok, out, err) = stack.run(&["qass", "watch", "on"]);
    assert!(ok && out.trim() == "watching resumed", "{out} {err}");
    let told = || {
        stack
            .log("qass")
            .contains("watching on: start your watcher")
    };
    assert!(
        stack.wait(told) || stack.wait(told),
        "a sleeping qass was never woken and told: {:?}",
        stack.log("qass")
    );
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stack.log("qass").matches("watching on:").count(), 1);
    assert!(!stack.asleep().contains(&qass), "qass is still asleep");
    assert!(stack.asleep().contains(&other), "a parked tab was woken");
}

/// The eye opened while qass is busy, and the daemon moved onto its binary again before qass
/// came to rest (`qoop restart`, or a daemon that died): the line is not lost with the old
/// process, and it is typed once qass takes words, once.
#[test]
fn watching_turned_on_reaches_a_busy_qass_across_a_restart() {
    let stack = Stack::start();
    let qass = stack.agent("qass", "qass", None);
    stack.run(&["qass", "watch", "off"]);
    let (ok, _, err) = stack.run(&["send", &qass, "BUSY", "--enter"]);
    assert!(ok, "send failed: {err}");
    assert!(
        stack.wait(|| stack.state_of(&qass) == "working"),
        "qass never got busy"
    );
    let (ok, out, err) = stack.run(&["qass", "watch", "on"]);
    assert!(ok && out.trim() == "watching resumed", "{out} {err}");
    let (ok, out, err) = stack.run(&["restart"]);
    assert!(ok, "restart failed: {out} {err}");
    assert_eq!(
        stack.log("qass").matches("watching on:").count(),
        0,
        "typed while busy"
    );
    let (ok, _, err) = stack.run(&["send", &qass, "FREE", "--enter"]);
    assert!(ok, "send failed: {err}");
    let told = || {
        stack
            .log("qass")
            .contains("watching on: start your watcher")
    };
    assert!(
        stack.wait(told) || stack.wait(told),
        "the line was lost across the restart: {:?}",
        stack.log("qass")
    );
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stack.log("qass").matches("watching on:").count(), 1);
}

/// The line still untold when the daemon moves onto its binary, and the owner halfway through a
/// sentence in qass: who typed there is not carried over the move, so the new process waits
/// out the same 30 quiet seconds from its start before it types the line over his draft.
#[test]
fn watching_turned_on_waits_for_a_draft_across_a_restart() {
    let stack = Stack::start();
    let qass = stack.agent("qass", "qass", None);
    stack.run(&["qass", "watch", "off"]);
    stack.run(&["send", &qass, "BUSY", "--enter"]);
    assert!(
        stack.wait(|| stack.state_of(&qass) == "working"),
        "qass never got busy"
    );
    stack.run(&["qass", "watch", "on"]);
    stack.run(&["send", &qass, "FREE", "--enter"]);
    assert!(
        stack.wait(|| stack.state_of(&qass) == "idle"),
        "qass never came to rest"
    );
    let (ok, _, err) = stack.run(&["send", &qass, "half a sente"]);
    assert!(ok, "send failed: {err}");
    let (ok, out, err) = stack.run(&["restart"]);
    assert!(ok, "restart failed: {out} {err}");
    std::thread::sleep(Duration::from_secs(20));
    assert!(
        !stack.log("qass").contains("watching on:"),
        "typed over the draft: {:?}",
        stack.log("qass")
    );
}

/// A qass that is awake takes the line where it stands: a worker parked asleep in the same
/// space is not woken by the eye.
#[test]
fn watching_turned_on_leaves_a_parked_tab_asleep_beside_an_awake_qass() {
    let stack = Stack::start();
    let _qass = stack.agent("qass", "qass", None);
    let parked = stack.agent("qass", "parked", Some("sess-parked"));
    assert!(
        stack.wait(|| stack.run(&["sleep", "--tab", &parked]).0),
        "the parked tab never went to sleep"
    );
    stack.run(&["qass", "watch", "off"]);
    let (ok, out, err) = stack.run(&["qass", "watch", "on"]);
    assert!(ok && out.trim() == "watching resumed", "{out} {err}");
    let told = || {
        stack
            .log("qass")
            .contains("watching on: start your watcher")
    };
    assert!(
        stack.wait(told) || stack.wait(told),
        "an awake qass was never told: {:?}\n{}\n{}",
        stack.log("qass"),
        stack.run(&["agents"]).1,
        stack.run(&["ls", "--json"]).1
    );
    assert!(stack.asleep().contains(&parked), "the parked tab was woken");
}

#[test]
fn an_answer_goes_to_the_worker_when_it_is_the_same_conversation_and_through_qass_otherwise() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let cv = stack.agent("work", "cv", None);
    let shell = stack.open("qass", "qassshell", &[]);
    let qass = stack.agent("qass", "qass", None);

    // Route 1: the card names a session and the worker reports the same one.
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    // He must never send words he did not read: a stale view of the option is refused.
    let (ok, _, err) = stack.run(&[
        "ask",
        "answer",
        "E1",
        "--option",
        "b",
        "--seen",
        "Do something else.",
    ]);
    assert!(!ok && err.contains("changed since you read it"), "{err}");
    assert_eq!(stack.card_status("E1"), "open");
    let (ok, out, err) = stack.run(&["ask", "answer", "E1", "--option", "b"]);
    assert!(ok && out.contains("Answered"), "{out} {err}");
    assert!(
        stack.wait(|| stack
            .log("korea")
            .contains("He chose B: Do not run 0042 yet")),
        "the worker was not told: {:?}",
        stack.log("korea")
    );
    assert!(
        stack
            .log("korea")
            .starts_with("The owner answered E1: \"korea B\".")
    );
    assert_eq!(stack.card_status("E1"), "answered");
    assert_eq!(stack.log("qass"), "", "qass was not involved");

    // A worker that is mid-turn is never refused: the answer is kept (`queued`) and typed when
    // the worker is next at rest.
    let (ok, _, err) = stack.run(&["send", &korea, "BUSY", "--enter"]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.state_of(&korea) == "working"));
    add(&stack, &shell, "E5", &format!("T{korea}"), "korea");
    let (ok, out, err) = stack.run(&["ask", "answer", "E5", "--option", "a"]);
    assert!(ok && out.contains("Answered"), "{out} {err}");
    assert_eq!(stack.card_status("E5"), "queued");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !stack.log("korea").contains("The owner answered E5"),
        "typed into a turn that is running: {:?}",
        stack.log("korea")
    );
    let (ok, _, err) = stack.run(&["send", &korea, "FREE", "--enter"]);
    assert!(ok, "{err}");
    assert!(
        stack.wait(|| stack
            .log("korea")
            .contains("The owner answered E5: \"korea A\".")),
        "{:?}",
        stack.log("korea")
    );
    assert_eq!(stack.card_status("E5"), "answered");

    // Route 2: an agent that reports no session is answered through qass, never refused. qass
    // was typed into when its tab opened, so for 30 seconds the answer is kept.
    add(&stack, &shell, "E2", &format!("T{cv}"), "cv");
    let (ok, out, err) = stack.run(&["ask", "answer", "E2", "--option", "a"]);
    assert!(ok && out.contains("Passed"), "{out} {err}");
    assert_eq!(stack.card_status("E2"), "queued", "kept, not refused");
    assert_eq!(stack.log("qass"), "");
    assert_eq!(
        stack.log("cv"),
        "",
        "the worker is never typed to without its session"
    );

    // While qass works the answers wait too, and a second one lines up behind the first.
    let (ok, _, err) = stack.run(&["send", &qass, "BUSY", "--enter"]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.state_of(&qass) == "working"));
    add(&stack, &shell, "E3", "-", "h");
    let (ok, out, err) = stack.run(&["ask", "answer", "E3", "--text", "run", "it", "twice"]);
    assert!(ok && out.contains("Passed"), "{out} {err}");
    assert_eq!(stack.card_status("E3"), "queued");
    let (ok, _, err) = stack.run(&["send", &qass, "FREE", "--enter"]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.state_of(&qass) == "idle"));
    // Idle again, but someone just typed into it: still kept.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(stack.card_status("E2"), "queued");
    assert!(
        !stack.log("qass").contains("cv A"),
        "{:?}",
        stack.log("qass")
    );

    // At rest for over 30 seconds: each is typed to qass once, in the order they were answered.
    std::thread::sleep(Duration::from_secs(31));
    assert!(
        stack.wait(|| stack.log("qass").contains("h run it twice")),
        "{:?}",
        stack.log("qass")
    );
    let typed = stack.log("qass");
    let (first, second) = (
        typed.find("cv A").unwrap(),
        typed.find("h run it twice").unwrap(),
    );
    assert!(first < second, "in order: {typed:?}");
    assert_eq!(typed.matches("cv A").count(), 1, "once each: {typed:?}");
    assert_eq!(stack.card_status("E2"), "passed");
    assert_eq!(stack.card_status("E3"), "passed");
    // qass passes his answer on (`The owner answered E2`), in whichever tab: the card ends.
    let (ok, _, err) = stack.run(&["send", &korea, "RELAY E2", "--enter"]);
    assert!(ok, "{err}");
    assert!(
        stack.wait(|| stack.card_status("E2") == "closed"),
        "a passed card relayed by qass stayed {}",
        stack.card_status("E2")
    );

    // qass reports one it handled without a relay: the card ends. A second answer is refused.
    let said = stack.inside(&shell, "qoop ask close E3 --note 'qass answered'");
    assert!(said.contains("Closed"), "{said}");
    assert_eq!(stack.card_status("E3"), "closed");
    let (ok, _, err) = stack.run(&["ask", "answer", "E3", "--option", "a"]);
    assert!(!ok && err.contains("already closed"), "{err}");
}

#[test]
fn a_card_ends_when_the_owner_puts_it_away_or_its_worker_is_told_in_the_chat() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    let (ok, out, err) = stack.run(&["ask", "dismiss", "E1"]);
    assert!(ok && out.contains("Closed"), "{out} {err}");
    assert_eq!(stack.card_status("E1"), "closed");
    // qass passes his chat answer to the worker; the worker's prompt head names the card.
    add(&stack, &shell, "E2", &format!("T{korea}"), "korea");
    let (ok, _, err) = stack.run(&["send", &korea, "RELAY E2", "--enter"]);
    assert!(ok, "{err}");
    assert!(
        stack.wait(|| stack.card_status("E2") == "closed"),
        "the prompt head did not close the card"
    );
    let (_, out, _) = stack.run(&["ask", "ls", "--json"]);
    assert!(out.contains("answered in chat"), "{out}");
    // E20 is not E200: only the card a head names is closed as answered. The prompt itself may
    // still close it as taken, when it lands in a later second than the card (`prompt_taken`
    // compares whole seconds), so the reason is what is checked, not that it stays open.
    add(&stack, &shell, "E20", &format!("T{korea}"), "korea");
    let (ok, _, err) = stack.run(&["send", &korea, "RELAY E200", "--enter"]);
    assert!(ok, "{err}");
    std::thread::sleep(Duration::from_millis(800));
    let (_, out, _) = stack.run(&["ask", "ls", "--json"]);
    let all: serde_json::Value = serde_json::from_str(&out).expect("json");
    let e20 = ["live", "ended"]
        .iter()
        .flat_map(|k| all[k].as_array().cloned().unwrap_or_default())
        .find(|c| c["card"] == "E20")
        .expect("E20 is listed");
    assert_ne!(e20["note"], "answered in chat", "{out}");
}

/// qass relays his chat answer to whichever tab it likes, not only the card's own: the card
/// about the worker named in the head ends all the same while it is `open` (or `passed`, which
/// the long route test above shows). An answer he gave that waits to be typed (queued, here) is
/// not undone by a head that names its card.
#[test]
fn a_card_ends_when_its_answer_is_relayed_to_another_tab_but_never_while_queued() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let lead = stack.agent("work", "lead", Some("sess-lead"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    // The head lands in the lead's tab; the card is about korea.
    let (ok, _, err) = stack.run(&["send", &lead, "RELAY E1", "--enter"]);
    assert!(ok, "{err}");
    assert!(
        stack.wait(|| stack.card_status("E1") == "closed"),
        "a relay to another tab did not end the card"
    );
    let (_, out, _) = stack.run(&["ask", "ls", "--json"]);
    assert!(out.contains("answered in chat"), "{out}");

    // His answer to a busy worker is kept (queued): a head naming that card ends nothing.
    let (ok, _, err) = stack.run(&["send", &korea, "BUSY", "--enter"]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.state_of(&korea) == "working"));
    add(&stack, &shell, "E2", &format!("T{korea}"), "korea");
    let (ok, out, err) = stack.run(&["ask", "answer", "E2", "--option", "a"]);
    assert!(ok && out.contains("Answered"), "{out} {err}");
    assert_eq!(stack.card_status("E2"), "queued");
    let (ok, _, err) = stack.run(&["send", &lead, "RELAY E2", "--enter"]);
    assert!(ok, "{err}");
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(stack.card_status("E2"), "queued", "a queued answer stays");
}

/// An agent that is gone for longer than the grace (10 s, no knob) takes its tab's open cards
/// with it. Its conversation must be on disk before it starts, or the daemon never records the
/// id and has nothing to forget. A card of another agent stays, and so does the card of one
/// that came back to the same conversation inside the grace (a quick resume).
#[test]
fn a_card_ends_when_its_agent_has_left_its_tab() {
    let stack = Stack::start();
    let id = format!("left-{}-5a1c0de0", std::process::id());
    let projects = qoop_testkit::sandboxed("true")
        .get_envs()
        .find(|(k, _)| *k == "HOME")
        .and_then(|(_, v)| v.map(PathBuf::from))
        .expect("the sandbox has a home")
        .join(".claude/projects/-work-left");
    std::fs::create_dir_all(&projects).unwrap();
    let back_id = format!("back-{}-5a1c0de0", std::process::id());
    for id in [&id, &back_id] {
        std::fs::write(
            projects.join(format!("{id}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"uuid\":\"u0\",\"sessionId\":\"{id}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n"
            ),
        )
        .unwrap();
    }
    let korea = stack.agent("work", "korea", Some(&id));
    let back = stack.agent("work", "back", Some(&back_id));
    let cv = stack.agent("work", "cv", Some("sess-cv"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    add(&stack, &shell, "E2", &format!("T{cv}"), "cv");
    add(&stack, &shell, "E3", &format!("T{back}"), "back");
    // Quick: still inside the grace, nothing has happened.
    stack.send(&korea, "EXIT");
    // `back` quits and is started again on its conversation at once, inside the grace.
    stack.send(&back, "EXIT");
    std::thread::sleep(Duration::from_millis(500));
    let again = format!(
        "STUB_LOG={} STUB_SESSION={back_id} claude",
        stack.path("back.log").display()
    );
    let (ok, _, err) = stack.run(&["send", &back, &again, "--enter"]);
    assert!(ok, "{err}");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(stack.card_status("E1"), "open", "inside the grace");
    assert!(
        stack.wait(|| stack.card_status("E1") == "closed"),
        "the card of an agent that left stayed open: {}",
        stack.run(&["ask", "ls", "--json"]).1
    );
    let (_, out, _) = stack.run(&["ask", "ls", "--json"]);
    assert!(out.contains("its agent left"), "{out}");
    assert_eq!(stack.card_status("E2"), "open", "another agent's card");
    assert_eq!(stack.card_status("E3"), "open", "back inside the grace");
}

#[test]
fn a_newer_card_for_an_agent_replaces_the_old_and_a_closed_terminal_ends_it() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    // escalate.sh passes the bare number.
    let said = add(&stack, &shell, "E2", &korea, "korea");
    assert!(
        said.contains("replaced E1 korea: Run 0042 on staging now?"),
        "the add says which question it ended: {said}"
    );
    assert_eq!(stack.card_status("E1"), "closed", "superseded");
    assert_eq!(stack.card_status("E2"), "open");
    // A number is never used twice.
    let said = add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    assert!(said.contains("never used twice"), "{said}");
    // Its terminal ends: a question about an agent that is gone is stale, so the card ends.
    let (ok, _, err) = stack.run(&["close", &korea]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.card_status("E2") == "closed"));
}

/// An agent that takes a prompt in its tab (his answer there, or new work) has moved past the
/// question: qass's open card about it ends by itself.
#[test]
fn a_card_ends_when_its_agent_takes_a_prompt_in_its_tab() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    assert_eq!(stack.card_status("E1"), "open");
    // A card is ended only by a prompt after the second it was written in.
    std::thread::sleep(Duration::from_millis(1100));
    stack.send(&korea, "SAID only iOS, send it");
    assert!(
        stack.wait(|| stack.card_status("E1") == "closed"),
        "{}",
        stack.run(&["ask", "ls", "--json"]).1
    );
    assert!(
        stack
            .run(&["ask", "ls", "--json"])
            .1
            .contains("it took a prompt in its tab")
    );
}

/// `escalate.sh` as the skill installs it, its data folder filled in.
fn escalate(stack: &Stack) -> PathBuf {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/qass/escalate.sh"
    ))
    .expect("the skill's escalate.sh");
    let data = stack.path("qass-data");
    std::fs::create_dir_all(&data).unwrap();
    let path = stack.path("escalate.sh");
    std::fs::write(
        &path,
        src.replace("@QASS_DATA@", &data.display().to_string()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The skill's own door, run as qass runs it: from a terminal in qass's space, the card as a
/// heredoc. A scripted stand-in for qass, never a model.
#[test]
fn escalate_sh_keeps_a_block_card_and_done_ends_it_while_the_old_form_still_works() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let script = escalate(&stack);
    let sh = script.display();
    let card = stack.path("card.txt");

    // A block card is kept, and the options are echoed as before.
    let said = stack.inside(
        &shell,
        &format!("'{sh}' E1 {korea} later korea - < '{}'", card.display()),
    );
    assert!(
        said.contains("E1 Added") && said.contains("  A: run now"),
        "{said}"
    );
    assert!(
        said.contains("noted E1"),
        "a later card only says it is noted: {said}"
    );
    assert_eq!(stack.card_status("E1"), "open");

    // A now card is stored before the ring (the ring is the 3 second hold of today).
    let said = stack.inside(
        &shell,
        &format!("'{sh}' E2 - now other - < '{}'", card.display()),
    );
    assert!(
        said.contains("E2 Added") && said.contains("escalated E2"),
        "{said}"
    );

    // The old positional form is untouched: options echoed, nothing stored.
    let said = stack.inside(
        &shell,
        &format!("'{sh}' E3 {korea} later korea 'A: yes' 'B: no'"),
    );
    assert!(
        said.contains("  A: yes") && said.contains("noted E3"),
        "{said}"
    );
    assert_eq!(stack.card_status("E3"), "");

    // A forgotten heredoc under the `-` ends in a refusal, never a hang; without the `-` nothing is read.
    let said = stack.inside(
        &shell,
        &format!("{{ '{sh}' E4 {korea} later korea - < /dev/null; echo rc=$?; }}"),
    );
    assert!(
        said.contains("card was not stored")
            && said.contains("doing is missing")
            && said.contains("rc=2")
            && !said.contains("noted E4"),
        "{said}"
    );
    assert_eq!(stack.card_status("E4"), "");
    let said = stack.inside(&shell, &format!("sleep 3 | '{sh}' E5 {korea} later korea"));
    assert!(said.contains("noted E5"), "{said}");

    let said = stack.inside(
        &shell,
        &format!("sleep 8 | '{sh}' E6 {korea} later korea -"),
    );
    assert!(
        said.contains("no card arrived"),
        "an open empty pipe is a refusal: {said}"
    );
    assert_eq!(stack.card_status("E6"), "");

    // A daemon that cannot keep the card (here none runs): the card is shown as written, the
    // ring would still go, and the work is not stopped.
    let nowhere = tempfile::tempdir().unwrap();
    std::fs::set_permissions(nowhere.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let out = qoop_testkit::sandboxed("sh")
        .arg("-c")
        .arg(format!(
            "'{sh}' E7 - later korea - < '{}'; echo rc=$?",
            card.display()
        ))
        .env("QOOP_RUNTIME_DIR", nowhere.path())
        .env("XDG_CONFIG_HOME", nowhere.path().join("config"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                stack.path("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("QOOP_TERMINAL")
        .env_remove("QOOP_TOKEN")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("  A: run now")
            && said.contains("needs a restart of qoop")
            && said.contains("noted E7")
            && said.contains("rc=0"),
        "{said} {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // done ends the card; a card the inbox does not know does not stop the work.
    let said = stack.inside(&shell, &format!("'{sh}' done E1 chat"));
    assert!(said.contains("E1 Closed"), "{said}");
    assert_eq!(stack.card_status("E1"), "closed");
    let said = stack.inside(&shell, &format!("'{sh}' done E99"));
    assert!(said.contains("noted E99"), "{said}");
}

/// A `replaces:` line in the block ends those older cards that are still open E cards, through
/// the skill's own door, and nothing else: an answer he gave, an ended card, the card's own
/// number and a number the inbox never had stay as they are. A bad token stores nothing.
#[test]
fn a_card_that_replaces_others_ends_only_the_open_ones_it_names() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    let script = escalate(&stack);
    let sh = script.display();
    let card = stack.path("card.txt");
    add(&stack, &shell, "E1", "-", "one");
    add(&stack, &shell, "E3", "-", "three");
    add(&stack, &shell, "E4", "-", "four");
    let (ok, _, err) = stack.run(&["send", &korea, "BUSY", "--enter"]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.state_of(&korea) == "working"));
    add(&stack, &shell, "E2", &format!("T{korea}"), "korea");
    let (ok, out, err) = stack.run(&["ask", "answer", "E2", "--option", "a"]);
    assert!(ok && out.contains("Answered"), "{out} {err}");
    stack.inside(&shell, "qoop ask close E4 --note chat");
    let (status, note) = (stack.card_status("E2"), stack.card_status("E4"));
    assert_eq!((status.as_str(), note.as_str()), ("queued", "closed"));

    let block = format!(
        "replaces: E1 E2 E4 E10 E99\n{}",
        std::fs::read_to_string(&card).unwrap()
    );
    std::fs::write(stack.path("replacing.txt"), block).unwrap();
    let said = stack.inside(
        &shell,
        &format!(
            "'{sh}' E10 - later ten - < '{}'",
            stack.path("replacing.txt").display()
        ),
    );
    assert!(said.contains("E10 Added"), "{said}");
    assert!(
        said.contains("superseded E1 one:") && !said.contains("superseded E2"),
        "the add names what it ended: {said}"
    );
    assert_eq!(stack.card_status("E1"), "closed");
    assert_eq!(stack.card_status("E2"), "queued", "his answer stays");
    assert_eq!(stack.card_status("E3"), "open", "not named");
    assert_eq!(stack.card_status("E10"), "open", "its own number");
    assert_eq!(stack.card_status("E99"), "", "an unknown number");
    let (_, out, _) = stack.run(&["ask", "ls", "--json"]);
    assert!(out.contains("superseded by E10"), "{out}");
    assert!(
        out.contains("\"note\": \"chat\""),
        "E4 keeps its note: {out}"
    );

    // A token that is no card number stores nothing, and the format says why.
    std::fs::write(
        stack.path("bad.txt"),
        format!(
            "replaces: E3 soon\n{}",
            std::fs::read_to_string(&card).unwrap()
        ),
    )
    .unwrap();
    let said = stack.inside(
        &shell,
        &format!(
            "{{ '{sh}' E11 - later eleven - < '{}'; echo rc=$?; }}",
            stack.path("bad.txt").display()
        ),
    );
    assert!(
        said.contains("card was not stored") && said.contains("replaces") && said.contains("rc=2"),
        "{said}"
    );
    assert_eq!(stack.card_status("E11"), "");
    assert_eq!(stack.card_status("E3"), "open", "nothing was ended");
}

/// `escalate.sh new` takes the number and keeps the card in one call: cards in a row get their
/// own numbers, a lost counter still lands above every number the inbox keeps, a number given to
/// another question is refused (exit 4) and `new` goes on to the next, and a typo fix is taken.
#[test]
fn escalate_new_numbers_each_card_once_and_never_writes_over_another_question() {
    let stack = Stack::start();
    let shell = stack.open("qass", "qassshell", &[]);
    let script = escalate(&stack);
    let sh = script.display();
    let card = stack.path("card.txt");
    let card = card.display();
    let data = stack.path("qass-data");
    let new =
        |handle: &str| stack.inside(&shell, &format!("'{sh}' new - later {handle} - < '{card}'"));

    let numbers: Vec<String> = (0..3)
        .map(|_| {
            let said = new("h");
            assert!(said.contains("noted E"), "{said}");
            said.lines()
                .find_map(|l| l.strip_suffix(" Added"))
                .unwrap_or_else(|| panic!("{said}"))
                .to_string()
        })
        .collect();
    assert_eq!(numbers, ["E1", "E2", "E3"]);

    // The counter lost: the number still lands above every card the inbox keeps.
    std::fs::remove_file(data.join("next-card")).unwrap();
    assert!(new("h").contains("E4 Added"));

    // A number typed by hand over E4, about another question: refused, its own exit code.
    let said = stack.inside(
        &shell,
        &format!(
            "{{ qoop ask add E4 --agent - --handle other --urgency later < '{card}'; echo rc=$?; }}"
        ),
    );
    assert!(said.contains("taken") && said.contains("rc=4"), "{said}");
    // The same question again (a retry or a typo fix) is taken.
    let said = stack.inside(
        &shell,
        &format!(
            "{{ qoop ask add E4 --agent - --handle h --urgency later < '{card}'; echo rc=$?; }}"
        ),
    );
    assert!(said.contains("rc=0"), "{said}");

    // A counter behind and an inbox it cannot read: `new` meets E4, is refused, and takes E5.
    let blind = stack.path("blind");
    std::fs::create_dir_all(&blind).unwrap();
    std::fs::write(
        blind.join("qoop"),
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = \"ask ls\" ] && exit 1\nexec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_qoop")
        ),
    )
    .unwrap();
    std::fs::set_permissions(blind.join("qoop"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(data.join("next-card"), "4").unwrap();
    let said = stack.inside(
        &shell,
        &format!(
            "PATH='{}':$PATH '{sh}' new - later z - < '{card}'",
            blind.display()
        ),
    );
    assert!(
        said.contains("E5 Added") && said.contains("noted E5"),
        "{said}"
    );
    assert_eq!(stack.card_status("E4"), "open");
}

/// qass reconciles its notes against `qoop ask ls --json`: a card that ended among many others is
/// still in it, though the pane and the plain listing show only the last few.
#[test]
fn the_json_listing_holds_every_ended_card_and_the_plain_one_the_last_five() {
    let stack = Stack::start();
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    for n in 1..=7 {
        let said = stack.inside(
            &shell,
            &format!(
                "qoop ask add E{n} --agent - --handle h{n} --urgency later < '{}'",
                stack.path("card.txt").display()
            ),
        );
        assert!(said.contains("Added"), "{said}");
    }
    for n in 1..=7 {
        let (ok, _, err) = stack.run(&["ask", "dismiss", &format!("E{n}")]);
        assert!(ok, "{err}");
    }
    for n in 1..=7 {
        assert_eq!(stack.card_status(&format!("E{n}")), "closed", "E{n}");
    }
    let (_, plain, _) = stack.run(&["ask", "ls"]);
    assert_eq!(plain.matches("(ended)").count(), 5, "{plain}");
}

/// A question dialog still up after the wait drafts one card, "a question on its screen",
/// with no options; the next tool closes it. One answered before the wait gets none, a second
/// dialog on the same task gets a second card, and neither a permission menu nor qass's own
/// tab gets one.
#[test]
fn a_question_dialog_still_up_after_the_wait_drafts_one_card_and_a_tool_closes_it() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", None);
    let qass = stack.agent("qass", "qass", None);
    stack.send(&korea, "TASK");
    stack.send(&korea, "ASK");
    assert!(
        stack.wait(|| stack.drafts(&korea).len() == 1),
        "no draft: {:?}",
        stack.run(&["ask", "ls", "--json"]).1
    );
    let first = stack.drafts(&korea).remove(0);
    assert_eq!(first["status"], "open");
    assert!(
        first["body"]["asks"]
            .as_str()
            .is_some_and(|a| a.ends_with("has a question on its screen")),
        "{first}"
    );
    assert_eq!(first["body"]["doing"], "ship the login fix");
    assert_eq!(first["body"]["options"], serde_json::json!([]));
    let handle = first["body"]["handle"].as_str().unwrap().to_string();
    assert!(
        handle.starts_with("korea") && handle[5..].bytes().all(|b| b.is_ascii_digit()),
        "{handle}"
    );
    // Still up: still one card.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(stack.drafts(&korea).len(), 1);

    // The next tool closes it.
    stack.send(&korea, "TOOL");
    let card = first["card"].as_str().unwrap();
    assert!(stack.wait(|| stack.card_status(card) == "closed"));
    assert_eq!(stack.drafts(&korea)[0]["note"], "the dialog ended");

    // A dialog answered before the wait gets none.
    std::thread::sleep(Duration::from_millis(1100));
    stack.send(&korea, "ASK");
    stack.send(&korea, "TOOL");
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stack.drafts(&korea).len(), 1, "answered in time");

    // A second dialog on the same task is a spell of its own.
    stack.send(&korea, "ASK");
    assert!(stack.wait(|| stack.drafts(&korea).len() == 2));
    let second = stack.drafts(&korea).remove(1);
    assert_eq!(second["status"], "open");
    assert_ne!(second["card"], first["card"]);
    assert_eq!(second["body"]["handle"], handle.as_str());

    // A permission menu and qass's own dialog get none.
    stack.send(&korea, "TOOL");
    assert!(stack.wait(|| stack.drafts(&korea)[1]["status"] == "closed"));
    // A second on, so the spell is not the one the last card was opened in.
    std::thread::sleep(Duration::from_millis(1100));
    stack.send(&korea, "MENU");
    stack.send(&qass, "ASK");
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stack.drafts(&korea).len(), 2, "a menu");
    assert!(stack.drafts(&qass).is_empty(), "qass's own tab");
}

/// An agent with no task gets a draft too, its handle the agent's name and its terminal. A
/// blocked agent takes no words, even in the same conversation: his answer is typed to qass,
/// starting with that handle.
#[test]
fn a_dialog_draft_without_a_task_is_answered_through_qass_by_its_handle() {
    let stack = Stack::start();
    let cv = stack.agent("work", "cv", Some("sess-cv"));
    let _qass = stack.agent("qass", "qass", None);
    stack.send(&cv, "ASK");
    assert!(
        stack.wait(|| stack.drafts(&cv).len() == 1),
        "no draft: {:?}",
        stack.run(&["ask", "ls", "--json"]).1
    );
    let draft = stack.drafts(&cv).remove(0);
    let handle = format!("cv-t{cv}");
    assert_eq!(draft["body"]["handle"], handle.as_str());
    assert_eq!(draft["body"]["doing"], "no open task");
    let card = draft["card"].as_str().unwrap();
    let (ok, out, err) = stack.run(&["ask", "answer", card, "--text", "pick", "the", "second"]);
    assert!(ok && out.contains("Passed"), "{out} {err}");
    // qass was typed into when its tab opened: the answer waits out the quiet.
    std::thread::sleep(Duration::from_secs(31));
    assert!(
        stack.wait(|| stack.log("qass").contains("pick the second")),
        "{:?}",
        stack.log("qass")
    );
    assert!(
        stack
            .log("qass")
            .starts_with(&format!("{handle} pick the second")),
        "{:?}",
        stack.log("qass")
    );
    assert!(
        !stack.log("cv").contains("The owner answered"),
        "{:?}",
        stack.log("cv")
    );
}

/// The stage of the open task of `terminal`, as `qoop agents --json` lists it.
fn stage(stack: &Stack, terminal: &str) -> String {
    let (_, out, _) = stack.run(&["agents", "--json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap_or_default();
    rows.iter()
        .find(|r| r["terminal"].as_u64().map(|t| t.to_string()).as_deref() == Some(terminal))
        .and_then(|r| r["task"]["stage"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A turn that ends asking him in plain words, with no stage set, is still unanswered after
/// the wait: its task waits on him and his inbox gets one draft with those words, and his
/// prompt takes both back. A question he answers within the wait, a turn that asks nothing
/// and qass's own turn get none.
#[test]
fn a_question_left_in_a_turns_last_words_waits_on_him_and_drafts_a_card() {
    let stack = Stack::start();
    let link = stack.agent("work", "link", None);
    let qass = stack.agent("qass", "qass", None);
    stack.send(&link, "TASK");
    assert!(stack.wait(|| !stage(&stack, &link).is_empty()));
    let began = stage(&stack, &link);
    // What a remote's watch prints, read over ssh by the Mac's watcher.
    let mut watch = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
    watch
        .args(["agents", "--watch", "--json"])
        .env("QOOP_RUNTIME_DIR", stack.dir.path())
        .env("XDG_CONFIG_HOME", stack.dir.path().join("config"))
        .stdout(Stdio::piped());
    for var in ["QOOP_TERMINAL", "QOOP_TOKEN", "QORO_TERMINAL", "QORO_TOKEN"] {
        watch.env_remove(var);
    }
    let mut watch = watch.spawn().expect("watch");
    stack.send(
        &link,
        "DONE Logo fixed. Istersen bunu ayri bir is olarak guncellerim.",
    );
    assert!(
        stack.wait(|| stack.drafts(&link).len() == 1),
        "no draft: {:?}",
        stack.run(&["ask", "ls", "--json"]).1
    );
    let draft = stack.drafts(&link).remove(0);
    assert_eq!(draft["status"], "open");
    assert_eq!(
        draft["body"]["asks"],
        "Logo fixed. Istersen bunu ayri bir is olarak guncellerim."
    );
    assert_eq!(stage(&stack, &link), "waiting-human");
    // The watch said it although the state held: the row carries the task.
    std::thread::sleep(Duration::from_millis(500));
    let _ = watch.kill();
    let printed = watch.wait_with_output().expect("watch output").stdout;
    let last = String::from_utf8_lossy(&printed)
        .lines()
        .last()
        .unwrap_or_default()
        .to_string();
    assert!(last.contains("\"stage\":\"waiting-human\""), "{last}");

    // The harness's notice of its own background work is no answer: both stay.
    let card = draft["card"].as_str().unwrap().to_string();
    stack.send(
        &link,
        "SAID <task-notification>build done</task-notification>",
    );
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(stack.card_status(&card), "open");
    assert_eq!(stage(&stack, &link), "waiting-human");

    // His prompt in the tab answers it: the stage goes back and the draft closes.
    stack.send(&link, "SAID yes do it");
    assert!(stack.wait(|| stack.card_status(&card) == "closed"));
    assert_eq!(stage(&stack, &link), began);

    // Answered within the wait: nothing.
    std::thread::sleep(Duration::from_millis(1100));
    stack.send(&link, "DONE Merge PR #12 now?");
    stack.send(&link, "SAID merge");
    // A turn that asks nothing: nothing.
    stack.send(&link, "DONE Merged and deployed.");
    // qass's own turn: nothing.
    stack.send(&qass, "TASK");
    stack.send(&qass, "DONE Dub A mi B mi?");
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(
        stack.drafts(&link).len(),
        1,
        "answered in time, or no question"
    );
    assert_eq!(stage(&stack, &link), began);
    assert!(stack.drafts(&qass).is_empty(), "qass's own tab");

    // Waiting by its own word, its last line no question: the card says what it waits on,
    // never that line.
    stack.send(&link, "WAIT");
    stack.send(&link, "DONE Merged and deployed.");
    assert!(stack.wait(|| stack.drafts(&link).len() == 2));
    assert_eq!(
        stack.drafts(&link)[1]["body"]["asks"],
        "link waits on you: ship the login fix"
    );
}

/// A task opened by an edit has the owner's own prompt for a goal: the draft of its wait shows
/// neither as the question nor as what it does, and a prompt that reaches the agent after the
/// wait began closes the draft and keeps the next turn end from filing another.
#[test]
fn a_draft_never_shows_the_owners_prompt_and_closes_when_the_agent_works_again() {
    let stack = Stack::start();
    let link = stack.agent("work", "link", None);
    let _qass = stack.agent("qass", "qass", None);
    let words = "this is not the average you idiot follow the real rate";
    stack.send(&link, &format!("SAID {words}"));
    stack.send(&link, "EDIT");
    assert!(stack.wait(|| !stage(&stack, &link).is_empty()));
    stack.send(&link, "WAIT");
    stack.send(&link, "DONE Merged and deployed.");
    assert!(
        stack.wait(|| stack.drafts(&link).len() == 1),
        "no draft: {:?}",
        stack.run(&["ask", "ls", "--json"]).1
    );
    let draft = stack.drafts(&link).remove(0);
    assert!(!draft["body"].to_string().contains("idiot"), "{draft}");
    assert_eq!(draft["body"]["asks"], "link waits on you");
    let card = draft["card"].as_str().unwrap().to_string();

    // A tool: the agent works again, the draft ends.
    std::thread::sleep(Duration::from_millis(1100));
    stack.send(&link, "TOOL");
    assert!(stack.wait(|| stack.card_status(&card) == "closed"));
    // A prompt after the wait began, then a turn end asking nothing: no second draft.
    stack.send(&link, "SAID keep going");
    stack.send(&link, "DONE Still on it.");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(stack.drafts(&link).len(), 1, "no new draft");
}

/// A listing larger than one frame (64 KB) still arrives: 60 cards with long fields are over
/// 90 KB of JSON, and the daemon used to drop the connection instead of answering.
#[test]
fn a_json_listing_larger_than_one_frame_arrives_whole() {
    let stack = Stack::start();
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    let long = "x".repeat(190);
    let card = format!(
        "asked: {long}\ndoing: {long}\nwhy: {long}\nasks: {long}?\n\
         A: run now | {long} | {long}\nB: wait | {long} | {long}\npick: B | {long}\n"
    );
    std::fs::write(stack.path("long.txt"), card).unwrap();
    let said = stack.inside(
        &shell,
        &format!(
            "for n in $(seq 1 60); do qoop ask add E$n --agent - --handle h$n --urgency later < '{}' || echo FAILED; done",
            stack.path("long.txt").display()
        ),
    );
    assert!(!said.contains("FAILED"), "{said}");
    let (ok, out, err) = stack.run(&["ask", "ls", "--json"]);
    assert!(ok, "{err}");
    assert!(out.len() > 65_536, "only {} bytes", out.len());
    let all: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(all["live"].as_array().map(Vec::len), Some(60));
}

/// Another qoop terminal never types into qass's tab (the one in qass's folder): typed there,
/// a worker's note read as the owner's words, and one landed in his half-written message.
/// From no terminal (his own shell, `qoop task start`) it is typed as before.
#[test]
fn another_terminal_never_types_into_qasss_tab() {
    let stack = Stack::start();
    let worker = stack.open("work", "worker", &[]);
    let qtab = stack.qass_tab(&worker);
    let said = stack.inside(
        &worker,
        &format!("{{ qoop prompt {qtab} WORKER_NOTE_1; echo rc=$?; }}"),
    );
    assert!(
        said.contains("nothing was typed into qass") && said.contains("rc=1"),
        "{said}"
    );
    let said = stack.inside(
        &worker,
        &format!("{{ qoop send {qtab} --key enter; echo rc=$?; }}"),
    );
    assert!(
        said.contains("nothing was typed into qass") && said.contains("rc=1"),
        "{said}"
    );
    let said = stack.inside(
        &worker,
        &format!("{{ env -u QOOP_TOKEN qoop prompt {qtab} WORKER_NOTE_2; echo rc=$?; }}"),
    );
    assert!(
        said.contains("nothing was typed into qass") && said.contains("rc=1"),
        "one variable dropped: {said}"
    );
    let (ok, _, err) = stack.run(&["prompt", &qtab, "echo OWNER_LINE_3"]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.run(&["read", &qtab]).1.contains("OWNER_LINE_3")));
    let screen = stack.run(&["read", &qtab]).1;
    assert!(!screen.contains("WORKER_NOTE"), "{screen}");
}

/// qass is held to no count of writes: nine prompts and a text send in a row to one tab, from
/// its own tab, all go through. Only a person at the tab holds a write back.
#[test]
fn qass_writes_to_one_tab_as_often_as_it_needs() {
    let stack = Stack::start();
    let worker = stack.open("work", "worker", &[]);
    let qtab = stack.qass_tab(&worker);
    let said = stack.inside(
        &qtab,
        &format!(
            "{{ for n in $(seq 1 9); do qoop prompt {worker} \"true $n\" || echo REFUSED; done; \
             qoop send {worker} --enter true || echo REFUSED; }}"
        ),
    );
    assert!(
        !said.contains("REFUSED") && !said.contains("ceiling"),
        "{said}"
    );
}

/// A card qass writes on a worker with `--via-qass` (a plan the owner approves) sits on that
/// worker, but his answer goes to qass, which records it, never straight to the worker.
#[test]
fn a_via_qass_card_on_a_worker_is_answered_through_qass() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    let said = stack.inside(
        &shell,
        &format!(
            "qoop ask add E9 --agent T{korea} --handle plan --urgency later --via-qass < '{}'",
            stack.path("card.txt").display()
        ),
    );
    assert!(said.contains("Added"), "{said}");
    let (ok, out, err) = stack.run(&["ask", "answer", "E9", "--option", "a"]);
    assert!(
        ok && out.contains("Passed"),
        "to qass, not the worker: {out} {err}"
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !stack.log("korea").contains("The owner answered E9"),
        "the worker was typed to: {:?}",
        stack.log("korea")
    );
}

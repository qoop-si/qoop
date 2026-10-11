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
/// line it is sent, and flips its state on `BUSY` and `FREE`.
const STUB: &str = r#"#!/bin/sh
if [ -n "$STUB_SESSION" ]; then qoop hook-report --state idle --session-id "$STUB_SESSION"; else qoop hook-report --state idle; fi
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$STUB_LOG"
  case "$line" in
    BUSY) qoop hook-report --state working ;;
    FREE) qoop hook-report --state idle ;;
    RELAY*) qoop hook-report --state idle --event user --label "The owner answered ${line#RELAY }: in the chat" ;;
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
            .env("SHELL", "/bin/sh")
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
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"));
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

    fn state_of(&self, terminal: &str) -> String {
        let (_, out, _) = self.run(&["agents"]);
        out.lines()
            .find(|l| l.starts_with(&format!("term={terminal} ")))
            .and_then(|l| l.split("state=").nth(1))
            .and_then(|s| s.split(' ').next())
            .unwrap_or("")
            .to_string()
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

    // qass reports it handled: the card ends. A second answer is refused.
    let said = stack.inside(&shell, "qoop ask close E2 --note 'qass answered'");
    assert!(said.contains("Closed"), "{said}");
    assert_eq!(stack.card_status("E2"), "closed");
    let (ok, _, err) = stack.run(&["ask", "answer", "E2", "--option", "a"]);
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
    // E20 is not E2: only the card a head names is closed.
    add(&stack, &shell, "E20", &format!("T{korea}"), "korea");
    let (ok, _, err) = stack.run(&["send", &korea, "RELAY E200", "--enter"]);
    assert!(ok, "{err}");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(stack.card_status("E20"), "open");
}

#[test]
fn a_newer_card_for_an_agent_replaces_the_old_and_a_closed_terminal_closes_its_card() {
    let stack = Stack::start();
    let korea = stack.agent("work", "korea", Some("sess-korea"));
    let shell = stack.open("qass", "qassshell", &[]);
    let _qass = stack.agent("qass", "qass", None);
    add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    add(&stack, &shell, "E2", &format!("T{korea}"), "korea");
    assert_eq!(stack.card_status("E1"), "closed", "superseded");
    assert_eq!(stack.card_status("E2"), "open");
    // A number is never used twice.
    let said = add(&stack, &shell, "E1", &format!("T{korea}"), "korea");
    assert!(said.contains("never used twice"), "{said}");
    // Its terminal ends: the card ends with it.
    let (ok, _, err) = stack.run(&["close", &korea]);
    assert!(ok, "{err}");
    assert!(stack.wait(|| stack.card_status("E2") == "closed"));
    let (_, out, _) = stack.run(&["ask", "ls", "--json"]);
    assert!(out.contains("agent closed"), "{out}");
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

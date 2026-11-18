//! An agent's own menu on its row, for a person's connection only, and the guarded answer.
//!
//! A stand-in named `aider` (an agent the daemon knows, with no screen rules of its own) draws
//! a permission prompt, and a hook report says it is blocked. It reads lines: `B` draws the
//! same prompt over another command, anything else is printed back as `got:<line>`, so a key
//! typed into its line shows up in front of the next word. Compiled rather than a script,
//! because the daemon names the agent after the process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use qoop_runtime::control::{AgentMenu, AgentRow, ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

fn stand_in(dir: &Path) -> PathBuf {
    let source = dir.join("aider.rs");
    let program = r#"use std::io::BufRead;
fn prompt(command: &str) {
    println!();
    println!("  Bash command");
    println!("    {command}");
    println!("  Do you want to proceed?");
    println!("  > 1. Yes");
    println!("    2. No");
}
fn main() {
    prompt("ls /tmp");
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "B" => prompt("rm -rf ~/work"),
            other => println!("got:{other}"),
        }
    }
}
"#;
    std::fs::write(&source, program).expect("write");
    let built = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(dir.join("aider"))
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    dir.join("aider")
}

async fn row(client: &mut TestClient, terminal: u32) -> AgentRow {
    // Boxed, like every control call here: their futures held across awaits overflowed the
    // test thread's stack in a debug build.
    match Box::pin(client.control(&ControlRequest::ListAgents { id: "l".into() }, PATIENCE)).await {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .find(|row| row.terminal == terminal)
            .expect("the terminal is listed"),
        other => panic!("no listing: {other:?}"),
    }
}

/// Say through the hook that the agent in `terminal` is blocked.
async fn block(hook: &mut TestClient, terminal: u32, token: &str) {
    let reported = Box::pin(hook.control(
        &ControlRequest::ReportAgentState {
            from: None,
            id: "r".into(),
            terminal,
            token: token.to_string(),
            state: "blocked".into(),
            visible_blocker: true,
            visible_idle: false,
            visible_working: false,
            clock: 0,
            session: None,
            event: None,
            label: None,
            said: None,
        },
        PATIENCE,
    ))
    .await;
    assert!(
        matches!(reported, Some(ControlResponse::AgentStateReported { .. })),
        "the hook report was refused: {reported:?}"
    );
}

/// The menu on a person's row once the agent reads blocked with a menu other than `not`,
/// and the row's generation. Blocked is said again whenever the screen's own reading took it
/// back.
async fn blocked_menu(
    hook: &mut TestClient,
    person: &mut TestClient,
    terminal: u32,
    token: &str,
    not: Option<u64>,
) -> (AgentMenu, Option<u64>) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let row = row(person, terminal).await;
        if row.state != "blocked" {
            block(hook, terminal, token).await;
        } else if let Some(menu) = row.menu.clone().filter(|m| Some(m.seal) != not) {
            return (menu, row.generation);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no menu on the row: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn answer(
    client: &mut TestClient,
    terminal: u32,
    generation: Option<u64>,
    menu: &AgentMenu,
) -> Option<ControlResponse> {
    Box::pin(client.control(
        &ControlRequest::AnswerMenu {
            id: "a".into(),
            terminal,
            generation,
            seal: menu.seal,
            key: "1".into(),
        },
        PATIENCE,
    ))
    .await
}

fn refused(response: &Option<ControlResponse>, code: &str) -> bool {
    matches!(response, Some(ControlResponse::Failed { reason, .. }) if reason == code)
}

#[tokio::test]
async fn a_menu_reaches_a_persons_row_only_and_is_answered_once_while_it_is_there() {
    let dir = tempfile::tempdir().expect("tempdir");
    let aider = stand_in(dir.path());
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut owner = d.attach("owner").await;
    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    let mut program = d.attach("qoop-agents").await;

    owner
        .input_to(terminal, format!("'{}'\n", aider.display()).as_bytes())
        .await;
    assert!(
        owner
            .wait_for_text_on(terminal, "\n    ls /tmp\n", PATIENCE)
            .await
    );
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !d.agent(terminal).is_some_and(|a| {
        matches!(&a.identity, qoop_domain::AgentIdentity::Known { program, .. } if program == "aider")
    }) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the stand in was never read as aider: {:?}",
            d.agent(terminal)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A person's row shows the menu; a program's never does, not even in its JSON.
    let (shown, generation) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    assert_eq!(shown.question, "Do you want to proceed?");
    let labels: Vec<&str> = shown.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["Yes", "No"]);
    let listed = row(&mut program, terminal).await;
    assert_eq!(listed.menu, None);
    let json = serde_json::to_string(&listed).expect("encode");
    assert!(
        !json.contains("\"menu\":"),
        "a program's row carries a menu: {json}"
    );

    // Another conversation in the tab: refused.
    let other = answer(
        &mut ui,
        terminal,
        Some(generation.map_or(1, |g| g + 1)),
        &shown,
    )
    .await;
    assert!(refused(&other, "already_answered"), "{other:?}");

    // The menu changed under the person: the same words over another command, the agent still
    // blocked. The answer to the menu shown is refused, and nothing is typed: a 1 would stand
    // in front of the next line the stand in reads.
    owner.input_to(terminal, b"B\n").await;
    let (fresh, generation) =
        blocked_menu(&mut owner, &mut ui, terminal, &token, Some(shown.seal)).await;
    let stale = answer(&mut ui, terminal, generation, &shown).await;
    assert!(refused(&stale, "already_answered"), "{stale:?}");
    owner.input_to(terminal, b"MARK\n").await;
    assert!(owner.wait_for_text_on(terminal, "got:MARK", PATIENCE).await);
    assert!(
        !owner
            .wait_for_text_on(terminal, "got:1MARK", Duration::from_millis(300))
            .await
    );

    // The menu now on the screen (two rows under it are still near the bottom). A program may
    // not answer it, even with its seal.
    let (now, generation) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    assert_eq!(now.seal, fresh.seal);
    let by_program = answer(&mut program, terminal, generation, &now).await;
    assert!(refused(&by_program, "not_a_person"), "{by_program:?}");

    // Two people answer it at once (the TUI and the phone): one answer is typed, the other is
    // refused, since the screen has not been read since the first.
    let mut phone = d.attach_phone().await;
    let (first, second) = tokio::join!(
        answer(&mut ui, terminal, generation, &now),
        answer(&mut phone, terminal, generation, &now),
    );
    let answered = [&first, &second]
        .iter()
        .filter(|r| matches!(r, Some(ControlResponse::MenuAnswered { answers: true, .. })))
        .count();
    let again = [&first, &second]
        .iter()
        .filter(|r| refused(r, "already_answered"))
        .count();
    assert_eq!((answered, again), (1, 1), "{first:?} {second:?}");
    owner.input_to(terminal, b"DONE\n").await;
    assert!(
        owner
            .wait_for_text_on(terminal, "got:1DONE", PATIENCE)
            .await
    );
    assert!(
        !owner
            .wait_for_text_on(terminal, "got:11DONE", Duration::from_millis(300))
            .await
    );
    // Typed as a person's input: the row says who typed last.
    let kind = row(&mut ui, terminal).await.typed_by.map(|t| t.kind);
    assert!(
        matches!(kind.as_deref(), Some("computer" | "phone")),
        "{kind:?}"
    );
}

/// A stand-in drawn as Claude Code draws a permission prompt, its third option the one that
/// asks for other instructions. It reads keys, not lines, as Claude Code does: `3` clears the
/// menu and rests on an input row (`>`), where it takes words until Enter and then shows them
/// as `got:<words>`, each control byte as `<xx>`; `M` draws the menu again; any other key shows
/// `key:<key>` with no menu. Named `aider` for the reason the one above is.
fn claude_shaped(dir: &Path) -> PathBuf {
    let source = dir.join("aider.rs");
    let program = r#"use std::io::{Read, Write};
const MENU: &str = "  Bash command\r\n    ls /tmp\r\n  Do you want to proceed?\r\n  > 1. Yes\r\n    2. Yes, and don't ask again for ls\r\n    3. No, and tell Claude what to do differently (esc)\r\n";
fn draw(text: &str) {
    let mut out = std::io::stdout();
    write!(out, "\x1b[2J\x1b[H{text}").unwrap();
    out.flush().unwrap();
}
fn main() {
    let _ = std::process::Command::new("stty").args(["-icanon", "-echo", "min", "1"]).status();
    draw(MENU);
    let (mut input, mut line) = (false, String::new());
    for byte in std::io::stdin().lock().bytes() {
        let b = byte.unwrap();
        if input {
            if b == b'\r' || b == b'\n' {
                draw(&format!("got:{line}\r\n"));
                line.clear();
                input = false;
            } else if b < 0x20 || b >= 0x7f {
                line.push_str(&format!("<{b:02x}>"));
            } else {
                line.push(b as char);
            }
            continue;
        }
        match b {
            b'3' => {
                input = true;
                draw("  Interrupted by user\r\n\r\n> ");
            }
            b'M' => draw(MENU),
            other => draw(&format!("key:{}\r\n", other as char)),
        }
    }
}
"#;
    std::fs::write(&source, program).expect("write");
    let built = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(dir.join("aider"))
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    dir.join("aider")
}

/// Start `program` in `terminal` and wait until the daemon reads it as aider.
async fn start_aider(d: &TestDaemon, owner: &mut TestClient, terminal: u32, program: &Path) {
    owner
        .input_to(terminal, format!("'{}'\n", program.display()).as_bytes())
        .await;
    assert!(
        owner
            .wait_for_text_on(terminal, "Do you want to proceed?", PATIENCE)
            .await
    );
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !d.agent(terminal).is_some_and(|a| {
        matches!(&a.identity, qoop_domain::AgentIdentity::Known { program, .. } if program == "aider")
    }) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the stand in was never read as aider: {:?}",
            d.agent(terminal)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn token_of(d: &TestDaemon, terminal: u32) -> String {
    d.terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string()
}

async fn answer_text(
    client: &mut TestClient,
    terminal: u32,
    generation: Option<u64>,
    menu: &AgentMenu,
    key: &str,
    text: &str,
) -> Option<ControlResponse> {
    Box::pin(client.control(
        &ControlRequest::AnswerMenuText {
            id: "t".into(),
            terminal,
            generation,
            seal: menu.seal,
            key: key.into(),
            text: text.into(),
        },
        PATIENCE,
    ))
    .await
}

#[tokio::test]
async fn own_words_go_after_the_tell_key_once_the_agent_rests_on_its_input_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = claude_shaped(dir.path());
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    let mut owner = d.attach("owner").await;
    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    start_aider(&d, &mut owner, terminal, &agent).await;

    let (shown, generation) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    let tells: Vec<bool> = shown.options.iter().map(|o| o.tell).collect();
    assert_eq!(tells, [false, false, true]);
    assert!(
        shown.context.iter().any(|r| r == "ls /tmp"),
        "{:?}",
        shown.context
    );

    // A plain answer takes no words: refused, and nothing typed.
    let plain = answer_text(&mut ui, terminal, generation, &shown, "1", "hello").await;
    assert!(refused(&plain, "no_text_field"), "{plain:?}");
    assert!(
        !owner
            .wait_for_text_on(terminal, "key:", Duration::from_millis(500))
            .await
    );

    // The key first, then, on the input row, the words cleaned of every control byte, then
    // Enter: had any of them come before the key, the stand in would show `key:`, and a byte
    // left in them would show as `<xx>` or end the line early.
    let words = "use \x1b[31mred\x1b[0m\r\nthen\u{9b}2J stop";
    let cleaned = "use [31mred[0m  then2J stop";
    assert_eq!(qoop_runtime::menu::clean_words(words), cleaned);
    let typed = answer_text(&mut ui, terminal, generation, &shown, "3", words).await;
    assert!(
        matches!(
            typed,
            Some(ControlResponse::MenuAnswered { answers: true, .. })
        ),
        "{typed:?}"
    );
    assert!(
        owner
            .wait_for_text_on(terminal, &format!("got:{cleaned}"), PATIENCE)
            .await
    );
    let screen = owner.screen(terminal).expect("a screen").lines.join("\n");
    assert!(
        !screen.contains("key:") && !screen.contains('<'),
        "{screen}"
    );
}

/// `menu_seal`, on every row, a program's too: the menu's seal for this turn of it. The same
/// question gone and asked again reads another, so a late `qoop send --seal` for the first
/// never answers the second; the menu's own seal, which `AnswerMenu` takes, stays the same.
#[tokio::test]
async fn the_same_question_asked_again_has_another_menu_seal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = claude_shaped(dir.path());
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    let mut owner = d.attach("owner").await;
    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    let mut program = d.attach("qoop-agents").await;
    start_aider(&d, &mut owner, terminal, &agent).await;

    let (first, _) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    let first_turn = row(&mut program, terminal)
        .await
        .menu_seal
        .expect("a program's row carries the seal");

    // The menu goes (any key but 3 and M), then the same one comes back.
    owner.input_to(terminal, b"x").await;
    assert!(owner.wait_for_text_on(terminal, "key:x", PATIENCE).await);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while row(&mut ui, terminal).await.menu.is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the menu never went"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    owner.input_to(terminal, b"M").await;
    let (again, _) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    assert_eq!(again.seal, first.seal, "the same menu");
    let second_turn = row(&mut program, terminal).await.menu_seal.expect("a seal");
    assert_ne!(second_turn, first_turn, "another turn of it");
}

/// A hand is for the turn of the question it was given on: the same question asked again is a
/// turn nobody handed, so the hand ends (and not as missed) rather than live on into it.
#[tokio::test]
async fn a_hand_ends_when_the_same_question_is_asked_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = claude_shaped(dir.path());
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    let mut owner = d.attach("owner").await;
    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    start_aider(&d, &mut owner, terminal, &agent).await;
    let (shown, generation) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;

    // Handed for another machine's qass, so no qass is needed here.
    let handed = Box::pin(ui.control(
        &ControlRequest::HandToQass {
            id: "h".into(),
            hands: vec![qoop_runtime::control::Hand {
                terminal,
                generation,
                seal: Some(shown.seal),
                away: Some("abc123".into()),
            }],
        },
        PATIENCE,
    ))
    .await;
    assert!(
        matches!(handed, Some(ControlResponse::Handed { .. })),
        "{handed:?}"
    );
    let first = row(&mut ui, terminal).await;
    assert!(first.with_qass, "{first:?}");

    // The same question again: a new turn, and the hand is over.
    owner.input_to(terminal, b"x").await;
    assert!(owner.wait_for_text_on(terminal, "key:x", PATIENCE).await);
    owner.input_to(terminal, b"M").await;
    let _ = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let now = row(&mut ui, terminal).await;
        if !now.with_qass {
            assert!(!now.qass_missed, "{now:?}");
            assert_ne!(now.menu_seal, first.menu_seal, "another turn");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hand lived on: {now:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn who_answered_is_on_a_persons_row_for_a_while_and_never_on_a_programs() {
    // Ten seconds in the product; shorter here, so the end is seen without the wait.
    qoop_runtime::menu::ANSWERED_FOR_MS.store(1500, std::sync::atomic::Ordering::Relaxed);
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = claude_shaped(dir.path());
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    let mut owner = d.attach("owner").await;
    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    let mut program = d.attach("qoop-agents").await;
    start_aider(&d, &mut owner, terminal, &agent).await;
    let (shown, generation) = blocked_menu(&mut owner, &mut ui, terminal, &token, None).await;
    assert!(!shown.context.is_empty());

    // What `qoop agents --json` reads, a program's listing: no menu, no context, while the
    // person's row has both.
    let listed = row(&mut program, terminal).await;
    let json = serde_json::to_string(&listed).expect("encode");
    for key in ["\"menu\":", "\"context\":", "\"answered\":"] {
        assert!(!json.contains(key), "a program's row carries {key} {json}");
    }

    let mut phone = d.attach_phone().await;
    let answered = answer(&mut phone, terminal, generation, &shown).await;
    assert!(
        matches!(
            answered,
            Some(ControlResponse::MenuAnswered { answers: true, .. })
        ),
        "{answered:?}"
    );
    let said = row(&mut ui, terminal).await.answered.expect("who answered");
    assert_eq!((said.label.as_str(), said.by.as_str()), ("Yes", "phone"));
    let listed = row(&mut program, terminal).await;
    assert_eq!(listed.answered, None);
    let json = serde_json::to_string(&listed).expect("encode");
    assert!(!json.contains("\"answered\":"), "{json}");

    // Then gone, and a change says so to a client that reads the row on one.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let ours = |e: &qoop_runtime::control::EventMessage| matches!(e, qoop_runtime::control::EventMessage::AgentChanged { terminal: t } if *t == terminal);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "still says who answered"
        );
        if ui
            .wait_for_event(ours, Duration::from_secs(1))
            .await
            .is_none()
        {
            continue;
        }
        ui.forget_events();
        if row(&mut ui, terminal).await.answered.is_none() {
            break;
        }
    }
}

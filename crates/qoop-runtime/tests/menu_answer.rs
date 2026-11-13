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

//! Milestone 3 gates: who is running in a terminal, and how sure we are.
//!
//! The pure rules are in `qoro-domain` and tested there. These are the ones that need a
//! real process to change under a real terminal, which is the half that no amount of unit
//! testing reaches.

use std::time::{Duration, Instant};

use qoro_domain::AgentIdentity;
use qoro_testkit::{PATIENCE, TestDaemon};

/// G3.3: a new foreground process is a new generation, and the previous evidence goes with
/// it.
///
/// A pid is not an identity. The interesting half is that the belief is cleared: carrying
/// it forward is how a finished agent stays "working" forever in the interface.
#[tokio::test]
async fn g3_3_a_new_foreground_process_mints_a_generation_and_clears_the_belief() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("agent").await;

    c.input_to(terminal, b"echo READY_$((6*7))\n").await;
    assert!(c.wait_for_text_on(terminal, "READY_42", PATIENCE).await);
    let first = d.agent(terminal).expect("an agent record");

    // A foreground process that is not the shell.
    c.input_to(terminal, b"sleep 5\n").await;
    let deadline = Instant::now() + PATIENCE;
    let mut second = first.clone();
    while Instant::now() < deadline {
        second = d.agent(terminal).expect("an agent record");
        if second.capability.generation > first.capability.generation {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        second.capability.generation > first.capability.generation,
        "a new foreground process did not mint a generation"
    );
    assert_eq!(
        second.belief.generation, second.capability.generation,
        "the belief was carried across a generation it does not describe"
    );
    assert!(
        matches!(second.identity, AgentIdentity::Known { .. }),
        "the foreground process has no identity: {:?}",
        second.identity
    );
    // The token is unchanged, which is ADR-015: it binds the terminal, not the generation.
    assert_eq!(
        first.capability.token, second.capability.token,
        "the token changed with the generation, so every later observation would be refused"
    );
}

/// G3.4: an in-place `exec` is detected, and it is the case a pid keyed identity misses
/// entirely.
///
/// The shell replaces itself with another program. Same pid, same process group, different
/// program. On macOS this arrives as a kqueue `NOTE_EXEC` and the bound is 100 ms.
#[tokio::test]
async fn g3_4_an_in_place_exec_is_detected() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("execer").await;

    c.input_to(terminal, b"echo READY_$((6*7))\n").await;
    assert!(c.wait_for_text_on(terminal, "READY_42", PATIENCE).await);

    // A subshell that becomes something else without forking again. The pid stays the same
    // across the exec, which is what makes this the interesting case.
    c.input_to(terminal, b"sh -c 'exec sleep 5' \n").await;
    let deadline = Instant::now() + PATIENCE;
    let mut program = None;
    while Instant::now() < deadline {
        if let Some(agent) = d.agent(terminal)
            && let AgentIdentity::Known { program: p, .. } = &agent.identity
            && p.contains("sleep")
        {
            program = Some(p.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        program.is_some(),
        "the program running in the terminal was never seen to change; last seen {:?}",
        d.agent(terminal).map(|a| a.identity)
    );
}

/// The token a terminal's child is launched with is the terminal's, and the child can read
/// it.
///
/// Without this the whole capability model is a value nobody has, and every observation
/// would have to be trusted on the strength of arriving.
#[tokio::test]
async fn a_terminals_child_is_launched_with_that_terminals_token() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("tokened").await;

    c.input_to(terminal, b"echo TOKEN_IS_$QORO_TOKEN\n").await;
    let expected = d.agent(terminal).expect("an agent record").capability.token;
    assert!(
        c.wait_for_text_on(
            terminal,
            &format!("TOKEN_IS_{}", expected.as_str()),
            PATIENCE
        )
        .await,
        "the child does not have this terminal's token"
    );

    // And a second terminal gets a different one, or the token proves nothing.
    let other = qoro_runtime::daemon::open_terminal(
        d.terminals_arc(),
        std::path::Path::new("/bin/sh"),
        None,
        d.warnings_arc(),
        None,
        d.scanner_arc(),
        d.agents_arc(),
        d.events(),
    )
    .expect("a second terminal");
    assert_ne!(
        expected, other.token,
        "two terminals share a token, so a claim from one is a claim from the other"
    );
}

/// G3.4, the case that needs the exec subscription and nothing else.
///
/// The shell replaces itself at the prompt. No fork, no new process group, no output: the
/// only thing that changes is the program behind a pid we are already watching. A fork
/// notification cannot see this, and neither can reading the foreground group, because the
/// foreground group is the same one it always was.
#[tokio::test]
async fn g3_4_a_shell_that_replaces_itself_is_detected_without_a_fork() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("selfexec").await;

    c.input_to(terminal, b"echo READY_$((6*7))\n").await;
    assert!(c.wait_for_text_on(terminal, "READY_42", PATIENCE).await);
    let before = d.agent(terminal).expect("an agent record");
    let leader_before = before.leader;

    c.input_to(terminal, b"exec sleep 5\n").await;

    let deadline = Instant::now() + PATIENCE;
    let mut after = before.clone();
    while Instant::now() < deadline {
        after = d.agent(terminal).expect("an agent record");
        if let AgentIdentity::Known { program, .. } = &after.identity
            && program.contains("sleep")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        after.leader, leader_before,
        "the pid changed, so this is not the in place case the gate is about"
    );
    match &after.identity {
        AgentIdentity::Known { program, .. } => assert!(
            program.contains("sleep"),
            "the program is still {program}, so an in place exec was missed"
        ),
        other => panic!("the terminal has no identity after an exec: {other:?}"),
    }
    assert!(
        after.capability.generation > before.capability.generation,
        "an exec did not mint a generation"
    );
}

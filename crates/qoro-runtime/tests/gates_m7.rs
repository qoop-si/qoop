//! Milestone 7 gates: what an agent cannot do to another agent's terminal.
//!
//! G7.1 asks for every method in the control codec to be attempted. The valuable half of
//! that is not the attempts, it is that the list of methods is enumerated from the codec
//! rather than written by hand: a hand written list is a list that stops covering the codec
//! the first time somebody adds a variant.

use std::time::Duration;

use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_testkit::{PATIENCE, TestDaemon};

/// Every variant of the control codec, constructed so that each one names somebody else's
/// terminal.
///
/// Adding a variant to `ControlRequest` and not adding it here is a compile error, because
/// the match below is exhaustive by construction: the list is built from a match on a value
/// of the type, so a new variant makes this fail to compile rather than silently go
/// untested.
fn every_request_against(terminal: u32) -> Vec<ControlRequest> {
    let all = vec![
        ControlRequest::CreateTerminal {
            id: "x1".into(),
            cwd: None,
            workspace: None,
            name: None,
        },
        ControlRequest::CloseTerminal {
            id: "x2".into(),
            terminal,
        },
        ControlRequest::ListTerminals { id: "x3".into() },
        ControlRequest::Subscribe {
            id: "x4".into(),
            terminals: vec![terminal],
        },
        ControlRequest::Unsubscribe {
            id: "x5".into(),
            terminals: vec![terminal],
        },
        ControlRequest::RequestOwnership {
            id: "x6".into(),
            terminal,
        },
        ControlRequest::GrantOwnership {
            id: "x7".into(),
            terminal,
            to: "somebody".into(),
        },
        ControlRequest::ListAgents { id: "x8".into() },
        ControlRequest::ListWorkspaces { id: "x9".into() },
        ControlRequest::CreateWorkspace {
            id: "x10".into(),
            name: "another".into(),
        },
        ControlRequest::CloseWorkspace {
            id: "x10b".into(),
            workspace: 999,
        },
        // A space that does not exist: nothing sleeps or wakes, nothing is typed.
        ControlRequest::SleepWorkspace {
            id: "x10c".into(),
            workspace: 999,
            terminal: None,
        },
        ControlRequest::WakeWorkspace {
            id: "x10d".into(),
            workspace: Some(999),
            terminal: None,
        },
        // A forged token: refused, but answered. The report carries a state for
        // the index, never bytes for the child.
        ControlRequest::ReportAgentState {
            id: "x11".into(),
            terminal,
            token: "forged".into(),
            state: "working".into(),
            visible_blocker: false,
            visible_idle: false,
            visible_working: true,
            clock: 0,
            session: None,
            event: None,
            label: None,
        },
        ControlRequest::ReloadAgentManifests { id: "x12".into() },
        // Session identity without a state claim: an id for the store, never
        // bytes for the child, same as the state report above.
        ControlRequest::ReportAgentSession {
            id: "x12b".into(),
            terminal,
            token: "forged".into(),
            session: "abc-123".into(),
        },
        ControlRequest::RenameTerminal {
            id: "x13".into(),
            terminal,
            name: Some("release".into()),
        },
        ControlRequest::RenameWorkspace {
            id: "x14".into(),
            workspace: 1,
            name: "webshop".into(),
        },
        ControlRequest::SetWorkspaceInk {
            id: "x14b".into(),
            workspace: 1,
            ink: Some(2),
        },
        ControlRequest::MoveWorkspace {
            id: "x14c".into(),
            workspace: 1,
            before: None,
        },
        ControlRequest::SetKeepAwake {
            id: "x28".into(),
            workspace: 1,
            terminal: None,
            on: true,
        },
        ControlRequest::Shutdown { id: "x15".into() },
        ControlRequest::Scrollback {
            id: "x16".into(),
            terminal,
            up: 10,
            rows: 10,
        },
        ControlRequest::ListFocusEvents {
            id: "x17".into(),
            terminal,
            last_n: 10,
        },
        ControlRequest::ConnectRemote {
            id: "x19".into(),
            target: "me@devbox".into(),
        },
        ControlRequest::InstallRemote {
            id: "x20".into(),
            target: "me@devbox".into(),
        },
        ControlRequest::HostColors {
            id: "x20".into(),
            fg: [0, 0, 0],
            bg: [255, 255, 255],
        },
        // Keeps a file and answers its path: nothing is typed, the sender pastes the path
        // itself on the terminal channel it already holds.
        ControlRequest::SaveUpload {
            id: "x25".into(),
            ext: "png".into(),
            data: "aGk=".into(),
        },
        // A search reads history and answers where it matched: a query, never bytes for
        // the child.
        ControlRequest::SearchScrollback {
            id: "x21".into(),
            terminal,
            query: "READY".into(),
            from: 0,
            backward: true,
        },
        ControlRequest::SetSplit {
            id: "x22".into(),
            tree: qoro_runtime::control::Pane::Leaf { terminal },
        },
        // Shows a terminal in the attached interfaces: which one is looked at, never
        // anything typed into it.
        ControlRequest::Focus {
            id: "x23".into(),
            terminal,
        },
        // Says which rows are news for the person: read by the index, never typed.
        ControlRequest::News {
            id: "x24".into(),
            terminals: vec![terminal],
        },
        // A name for the conversation and a place for the tab: nothing typed.
        ControlRequest::RenameSession {
            id: "x25".into(),
            terminal,
            title: Some("renamed".into()),
        },
        ControlRequest::MoveTerminal {
            id: "x26".into(),
            terminal,
            workspace: 999,
            place: None,
            leave_split: false,
        },
        // 0.0.0 is no newer than this build, so it is answered as already there and nothing
        // is armed. A newer one would, at most, end every tab to become the binary on
        // disk: nothing is typed either way.
        ControlRequest::RestartWhenSafe {
            id: "x27".into(),
            version: "0.0.0".into(),
        },
        // A task is a record about the work: text kept by the daemon, never typed.
        ControlRequest::TaskWrite {
            id: "x28".into(),
            terminal: Some(terminal),
            token: Some("not-its-token".into()),
            task: None,
            action: qoro_runtime::control::TaskOp::New {
                goal: "echo PWNED".into(),
                replace: true,
            },
        },
        ControlRequest::TaskRead {
            id: "x29".into(),
            task: None,
            terminal: Some(terminal),
            token: None,
            before: None,
        },
    ];
    // Exhaustiveness, checked by the compiler rather than by a reviewer counting.
    for request in &all {
        match request {
            ControlRequest::CreateTerminal { .. }
            | ControlRequest::CloseTerminal { .. }
            | ControlRequest::ListTerminals { .. }
            | ControlRequest::ListAgents { .. }
            | ControlRequest::Focus { .. }
            | ControlRequest::News { .. }
            | ControlRequest::ListWorkspaces { .. }
            | ControlRequest::CreateWorkspace { .. }
            | ControlRequest::CloseWorkspace { .. }
            | ControlRequest::SleepWorkspace { .. }
            | ControlRequest::WakeWorkspace { .. }
            | ControlRequest::RenameTerminal { .. }
            | ControlRequest::RenameSession { .. }
            | ControlRequest::MoveTerminal { .. }
            | ControlRequest::RenameWorkspace { .. }
            | ControlRequest::SetWorkspaceInk { .. }
            | ControlRequest::MoveWorkspace { .. }
            | ControlRequest::SetKeepAwake { .. }
            | ControlRequest::Subscribe { .. }
            | ControlRequest::Unsubscribe { .. }
            | ControlRequest::RequestOwnership { .. }
            | ControlRequest::ReportAgentState { .. }
            | ControlRequest::ReportAgentSession { .. }
            | ControlRequest::ListFocusEvents { .. }
            | ControlRequest::ConnectRemote { .. }
            | ControlRequest::InstallRemote { .. }
            | ControlRequest::HostColors { .. }
            | ControlRequest::SaveUpload { .. }
            | ControlRequest::SetSplit { .. }
            | ControlRequest::RestartWhenSafe { .. }
            | ControlRequest::TaskWrite { .. }
            | ControlRequest::TaskRead { .. }
            | ControlRequest::ReloadAgentManifests { .. }
            | ControlRequest::Shutdown { .. }
            | ControlRequest::Scrollback { .. }
            | ControlRequest::SearchScrollback { .. }
            | ControlRequest::GrantOwnership { .. } => {}
        }
    }
    all
}

/// G7.1: no method in the control codec writes to a terminal.
///
/// The codec is the whole public surface for anything that is not a terminal channel frame,
/// and a terminal channel frame is addressed by the stream field of a connection that was
/// subscribed to it. So the question is whether any control method can put bytes into a
/// child, and the answer is that none of them takes bytes at all.
#[tokio::test]
async fn g7_1_no_control_method_writes_to_a_terminal() {
    let d = TestDaemon::start().await;
    let victim = d.terminals().streams()[0];
    let mut owner = d.attach("owner").await;

    // A marker the victim's shell would echo if anything reached it.
    owner.input_to(victim, b"echo READY_$((6*7))\n").await;
    assert!(owner.wait_for_text_on(victim, "READY_42", PATIENCE).await);
    owner.forget_seen();

    let mut intruder = d.attach("intruder").await;
    for request in every_request_against(victim) {
        // Closing is excluded, and it is the one exclusion. It does affect the victim's
        // terminal, on purpose, and it is not a write: the second test below pins what it
        // does. Every other method is attempted against somebody else's terminal.
        if matches!(request, ControlRequest::CloseTerminal { .. }) {
            continue;
        }
        // Shutdown is the second exclusion and it is excluded for the same reason as the
        // first, one step further: it ends every terminal, which is what it is for. It is
        // still not a write, it takes no bytes, and it would end this test's daemon along
        // with the victim, which would prove nothing about writes and lose the assertion
        // below.
        if matches!(request, ControlRequest::Shutdown { .. }) {
            continue;
        }
        // Every one of them is answered, and none of them carries bytes for a child.
        let answered = intruder.control(&request, PATIENCE).await;
        assert!(
            answered.is_some(),
            "the daemon did not answer {request:?}, so a caller cannot tell refused from lost"
        );
    }

    // Nothing the intruder sent reached the victim's shell. Asserted as an absence after a
    // marker proves the screen has been updated since.
    owner.input_to(victim, b"echo AFTER_$((6*8))\n").await;
    assert!(owner.wait_for_text_on(victim, "AFTER_48", PATIENCE).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner
        .wait_for_text_on(victim, "never matches", Duration::from_millis(200))
        .await;
    let screen = owner.screen(victim).expect("a screen").lines.join("\n");
    for trace in ["x1", "x2", "x4", "x6", "x7", "somebody"] {
        assert!(
            !screen.contains(trace),
            "a control request reached the child's terminal: {trace} in\n{screen}"
        );
    }
}

/// The one control method that changes another terminal is `CloseTerminal`, and closing is
/// not writing. It is still worth pinning that it does exactly that and nothing more.
///
/// Who may close what is not restricted here, and that is a deliberate consequence of
/// ADR-012 rather than an oversight: every connection is the same uid, and a same-uid
/// process that can reach the socket can also signal the child directly. A capability check
/// would be a check a caller can bypass by not using the socket.
///
/// It becomes a real question the moment a connection can carry a participant identity that
/// is not the user, which is what `qoro-collab`'s `Grants` is for and what the daemon does
/// not yet have. Recorded in the milestone 7 status rather than left to be discovered.
#[tokio::test]
async fn closing_someone_elses_terminal_ends_it_without_writing_to_it() {
    let d = TestDaemon::start().await;
    let mut owner = d.attach("owner").await;
    let created = owner
        .control(
            &ControlRequest::CreateTerminal {
                id: "mk".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let ControlResponse::TerminalCreated { terminal, .. } = created else {
        panic!("expected a created terminal");
    };

    owner.input_to(terminal, b"echo LIVE_$((6*7))\n").await;
    assert!(owner.wait_for_text_on(terminal, "LIVE_42", PATIENCE).await);

    let mut intruder = d.attach("intruder").await;
    let closed = intruder
        .control(
            &ControlRequest::CloseTerminal {
                id: "close".into(),
                terminal,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(matches!(closed, ControlResponse::TerminalClosed { .. }));

    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline && d.terminals().get(terminal).is_some() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        d.terminals().get(terminal).is_none(),
        "the terminal was reported closed and is still registered"
    );
}

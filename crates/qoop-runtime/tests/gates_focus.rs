//! Focus gates: hook events reach the focus log, and nothing else does.
//!
//! The log is display only: admitted reports land in it, forgeries and
//! state-only reports do not, and it is read back oldest-first through
//! `ListFocusEvents`. A terminal that is gone takes its log with it.

use qoop_runtime::control::{ControlRequest, ControlResponse};

/// Admitted event reports are listed oldest-first; a forgery, a state-only
/// report and an out-of-set kind write nothing; an unknown terminal is a
/// refusal rather than an empty answer.
#[tokio::test]
async fn admitted_events_are_listed_and_nothing_else_is() {
    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut client = d.attach("hook").await;

    async fn report(
        client: &mut qoop_testkit::TestClient,
        id: &str,
        terminal: u32,
        token: String,
        event: Option<&str>,
        label: Option<&str>,
    ) -> Option<ControlResponse> {
        client
            .control(
                &ControlRequest::ReportAgentState {
                    from: None,
                    id: id.into(),
                    terminal,
                    token,
                    state: "working".into(),
                    visible_blocker: false,
                    visible_idle: false,
                    visible_working: true,
                    clock: 0,
                    session: None,
                    event: event.map(str::to_string),
                    label: label.map(str::to_string),
                },
                qoop_testkit::PATIENCE,
            )
            .await
    }

    async fn listed(
        client: &mut qoop_testkit::TestClient,
        terminal: u32,
        last_n: usize,
    ) -> Option<ControlResponse> {
        client
            .control(
                &ControlRequest::ListFocusEvents {
                    id: "list".into(),
                    terminal,
                    last_n,
                },
                qoop_testkit::PATIENCE,
            )
            .await
    }

    fn events_of(answer: Option<ControlResponse>) -> Vec<(String, String)> {
        match answer {
            Some(ControlResponse::FocusEvents { events, .. }) => {
                events.into_iter().map(|e| (e.kind, e.label)).collect()
            }
            other => panic!("a focus listing was expected: {other:?}"),
        }
    }

    // A forgery carrying an event is refused, and writes nothing.
    let refused = report(
        &mut client,
        "forged",
        terminal,
        "not-the-token".into(),
        Some("tool"),
        Some("Read stolen.rs"),
    )
    .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "a forged event was accepted: {refused:?}"
    );
    assert!(
        events_of(listed(&mut client, terminal, 50).await).is_empty(),
        "a forgery wrote the focus log"
    );

    // Two admitted events, in order.
    for (id, kind, label) in [
        ("e1", "tool", "Read foo.rs"),
        ("e2", "user", "why is this slow"),
    ] {
        let accepted = report(
            &mut client,
            id,
            terminal,
            token.clone(),
            Some(kind),
            Some(label),
        )
        .await;
        assert!(
            matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
            "{accepted:?}"
        );
    }

    // A state report with no event changes no row.
    let accepted = report(&mut client, "plain", terminal, token.clone(), None, None).await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "{accepted:?}"
    );

    // An out-of-set kind is admitted as a state report (the CLI is the
    // strict side; the daemon stays lenient) but writes no row.
    let accepted = report(
        &mut client,
        "odd",
        terminal,
        token.clone(),
        Some("started"),
        Some("noise"),
    )
    .await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "{accepted:?}"
    );

    assert_eq!(
        events_of(listed(&mut client, terminal, 50).await),
        vec![
            ("tool".to_string(), "Read foo.rs".to_string()),
            ("user".to_string(), "why is this slow".to_string()),
        ],
        "only admitted in-set events, oldest first"
    );

    // `last_n` takes the tail, not the head.
    assert_eq!(
        events_of(listed(&mut client, terminal, 1).await),
        vec![("user".to_string(), "why is this slow".to_string())],
        "the newest event, not the oldest"
    );

    // Zero asks for nothing, not the newest row.
    assert!(
        events_of(listed(&mut client, terminal, 0).await).is_empty(),
        "last_n zero listed a row"
    );

    // A long multi-line label is cut to one row by the daemon, end to end.
    let long = format!("line one\nline two {}", "x".repeat(200));
    let accepted = report(
        &mut client,
        "long",
        terminal,
        token.clone(),
        Some("tool"),
        Some(&long),
    )
    .await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "{accepted:?}"
    );
    let rows = events_of(listed(&mut client, terminal, 50).await);
    let (_, label) = rows.last().expect("the long label was listed");
    assert_eq!(
        *label,
        qoop_runtime::control::truncate_focus_label(&long),
        "the daemon truncates before pushing"
    );
    assert!(
        !label.contains('\n') && !label.contains('\r'),
        "a multi-line label broke its row: {label:?}"
    );

    // An unknown terminal is a refusal, not an empty answer.
    let missing = listed(&mut client, terminal + 10_000, 50).await;
    assert!(
        matches!(missing, Some(ControlResponse::Failed { .. })),
        "an unknown terminal listed events: {missing:?}"
    );
}

/// The per-terminal log holds fifty rows and then drops the oldest; a long
/// label is cut on a char boundary; forgetting the terminal drops the log.
#[test]
fn the_focus_log_is_capped_cut_and_forgotten() {
    use qoop_domain::{CapabilityToken, TerminalId};

    let registry = qoop_runtime::agents::AgentRegistry::new().expect("registry");
    registry.register(1, TerminalId::new("1"), CapabilityToken::new("t"));
    for i in 0..qoop_runtime::control::MAX_FOCUS_EVENTS + 5 {
        registry.push_focus_event(
            1,
            qoop_runtime::control::FocusEvent {
                kind: "tool".into(),
                label: format!("tool {i}"),
                at_unix: i as i64,
            },
        );
    }
    let kept = registry.focus_events(1);
    assert_eq!(
        kept.len(),
        qoop_runtime::control::MAX_FOCUS_EVENTS,
        "the log grows past its cap"
    );
    assert_eq!(
        kept[0].label, "tool 5",
        "the oldest rows go first: {kept:?}"
    );
    assert_eq!(
        kept[kept.len() - 1].label,
        format!("tool {}", qoop_runtime::control::MAX_FOCUS_EVENTS + 4),
        "the newest row survives"
    );

    // The daemon truncates before pushing; the registry only caps. A label
    // already at the limit passes through untouched.
    let at_limit = "x".repeat(qoop_runtime::control::MAX_FOCUS_LABEL_CHARS);
    let cut = qoop_runtime::control::truncate_focus_label(&at_limit);
    assert_eq!(cut, at_limit);

    registry.forget(1);
    assert!(
        registry.focus_events(1).is_empty(),
        "a forgotten terminal kept its log"
    );
}

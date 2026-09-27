//! Search gates: copy mode's history search against a real daemon and a real history on
//! disk.

use std::time::{Duration, Instant};

use qoro_runtime::control::{ControlRequest, ControlResponse};

#[tokio::test]
async fn a_history_search_finds_the_row_and_the_cell_and_says_when_there_is_none() {
    let d = qoro_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut client = d.attach("searcher").await;

    client
        .input_to(
            terminal,
            b"echo lead; echo xx-NEEDLE-1; for i in $(seq 1 300); do echo row-$i; done\n",
        )
        .await;
    assert!(
        client
            .wait_for_text_on(terminal, "row-300", qoro_testkit::PATIENCE)
            .await
    );
    // The history is written by a task of its own: wait until it holds the output.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut total = 0;
    while Instant::now() < deadline && total < 250 {
        if let Some(ControlResponse::Scrollback { total: t, .. }) = client
            .control(
                &ControlRequest::Scrollback {
                    id: "count".into(),
                    terminal,
                    up: 1,
                    rows: 1,
                },
                qoro_testkit::PATIENCE,
            )
            .await
        {
            total = t;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(total >= 250, "only {total} rows reached the history");

    let search =
        |id: &str, query: &str, from: u64, backward: bool| ControlRequest::SearchScrollback {
            id: id.into(),
            terminal,
            query: query.into(),
            from,
            backward,
        };
    // Backward from the newest row, lower case: smart case finds the capitals.
    let found = client
        .control(
            &search("s1", "needle", total - 1, true),
            qoro_testkit::PATIENCE,
        )
        .await;
    let Some(ControlResponse::ScrollbackMatch {
        row: Some(row),
        col,
        resume: None,
        ..
    }) = found
    else {
        panic!("the needle was not found: {found:?}");
    };
    // Read the row back through the ordinary scrollback and check the cell is the match.
    let back = client
        .control(
            &ControlRequest::Scrollback {
                id: "row".into(),
                terminal,
                up: (total - row) as usize,
                rows: 1,
            },
            qoro_testkit::PATIENCE,
        )
        .await;
    let Some(ControlResponse::Scrollback { lines, .. }) = back else {
        panic!("the matched row could not be read: {back:?}");
    };
    let cells: String = lines[0].chars().skip(col).take(6).collect();
    assert_eq!(cells, "NEEDLE", "row {row} cell {col}: {lines:?}");

    // A capital means exactly that, and forward from the match there is no other.
    let none = client
        .control(
            &search("s2", "Needle", total - 1, true),
            qoro_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(
            none,
            Some(ControlResponse::ScrollbackMatch {
                row: None,
                resume: None,
                ..
            })
        ),
        "{none:?}"
    );
    let after = client
        .control(
            &search("s3", "needle", row + 1, false),
            qoro_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(
            after,
            Some(ControlResponse::ScrollbackMatch { row: None, .. })
        ),
        "{after:?}"
    );

    // Nothing to search for, and a terminal nobody has, are refusals rather than silence.
    let empty = client
        .control(&search("s4", "", 0, true), qoro_testkit::PATIENCE)
        .await;
    assert!(
        matches!(empty, Some(ControlResponse::Failed { .. })),
        "{empty:?}"
    );
    let nobody = client
        .control(
            &ControlRequest::SearchScrollback {
                id: "s5".into(),
                terminal: 9_999,
                query: "x".into(),
                from: 0,
                backward: true,
            },
            qoro_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(nobody, Some(ControlResponse::Failed { .. })),
        "{nobody:?}"
    );
}

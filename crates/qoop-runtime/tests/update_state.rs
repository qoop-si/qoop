//! A daemon that cannot move (embedded, not started as `qoop daemon`) has nothing to offer:
//! asked whether a newer qoop is on disk, it says there is none rather than failing.

use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestDaemon};

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_cannot_move_has_nothing_newer_to_offer() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("update-state").await;
    let answer = c
        .control(&ControlRequest::UpdateState { id: "s".into() }, PATIENCE)
        .await;
    assert_eq!(
        answer,
        Some(ControlResponse::UpdateState {
            id: "s".into(),
            ready: None
        })
    );
}

//! A move nobody asked for by hand never ends a tab: one that cannot hand every tab over live
//! waits instead, and nothing is paused or ended on the way.
//!
//! Driven against a daemon in this process, which a real move would replace, so the binary it
//! is asked to become is a stand-in that says it is newer and can take tabs over. If the move
//! went ahead anyway it would be this test run that became the stand-in: the assertions
//! below are only reached when it did not.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestDaemon};

fn stand_in(dir: &Path) -> std::path::PathBuf {
    let exe = dir.join("qoop-next");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\ncase \"$1\" in --version) echo 'qoop 99.0.0';; --handoff-formats) echo {};; esac\n",
            qoop_runtime::handoff::FORMAT
        ),
    )
    .expect("the stand-in");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    exe
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tab_that_cannot_be_handed_over_holds_a_move_that_ends_no_tab() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let exe = stand_in(scratch.path());
    let d = TestDaemon::start_moving_onto(Path::new("/bin/sh"), Some(&exe)).await;
    // The daemon can no longer say tab 1 has a pty it could pass on.
    d.agents_arc().detach_master(1);

    let mut c = d.attach("keep-tabs").await;
    let answer = c
        .control(
            &ControlRequest::RestartWhenSafe {
                id: "move".into(),
                version: "99.0.0".into(),
                keep_tabs: true,
                quiet: false,
            },
            PATIENCE,
        )
        .await;
    match answer {
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => assert!(why.contains("tab 1 cannot be handed over live"), "{why}"),
        other => panic!("not held for the tab: {other:?}"),
    }

    // Nothing was ended, and the gate is down: the next tab opens.
    assert!(d.terminals().get(1).is_some(), "the tab was ended");
    let created = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "after".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(created, Some(ControlResponse::TerminalCreated { .. })),
        "{created:?}"
    );
}

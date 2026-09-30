//! A picture attached on the phone: the daemon keeps it where only its user can read it,
//! under a name it makes, and a paste of its path reaches the program with the bracketed
//! paste markers exactly when the program asked for them. Claude Code attaches a pasted
//! picture path and keeps a typed one as text, so the markers are the feature.

use std::os::unix::fs::PermissionsExt;

use base64::Engine as _;
use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_runtime::daemon::ClientWire;
use qoro_testkit::{PATIENCE, TestDaemon};

fn upload(ext: &str, data: String) -> ControlRequest {
    ControlRequest::SaveUpload {
        id: format!("u-{ext}-{}", data.len()),
        ext: ext.into(),
        data,
    }
}

#[tokio::test]
async fn an_upload_is_kept_private_under_a_name_the_daemon_makes() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("phone").await;
    let bytes = b"\x89PNG not really".to_vec();
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let answer = c
        .control(&upload("png", data), PATIENCE)
        .await
        .expect("an answer");
    let ControlResponse::Uploaded { path, .. } = answer else {
        panic!("not kept: {answer:?}");
    };
    let path = std::path::PathBuf::from(path);
    assert!(
        path.starts_with(d.dir()),
        "{} is outside the daemon's own dir",
        path.display()
    );
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "readable by others: {mode:o}");

    // Not a picture, or too big: refused before anything is written.
    for request in [
        upload("sh", "aGk=".into()),
        upload("../x", "aGk=".into()),
        upload(
            "jpg",
            "A".repeat(qoro_runtime::daemon::MAX_UPLOAD / 3 * 4 + 8),
        ),
    ] {
        let answer = c.control(&request, PATIENCE).await.expect("an answer");
        assert!(
            matches!(answer, ControlResponse::Failed { .. }),
            "kept: {answer:?}"
        );
    }
    let kept = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
    assert_eq!(kept, 1, "a refused upload left a file behind");
}

#[tokio::test]
async fn a_paste_wears_the_markers_only_when_the_program_asked() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("phone").await;
    // `cat -v` shows the escape bytes it reads as `^[`, so the markers are visible.
    // The mode is on once the marker after it is on screen: the engine reads in order.
    c.input(b"printf '\\033[?2004h'; echo ON_$((6*7)); cat -v\n")
        .await;
    assert!(c.wait_for_text("ON_42", PATIENCE).await);
    // Escapes inside the paste are dropped, so no end marker survives, whole or spliced
    // back together from the pieces around a whole one.
    c.send(&ClientWire::Paste {
        bytes: b"/tmp/a.png\x1b[2\x1b[201~01~rm".to_vec(),
    })
    .await;
    c.input(b"\n").await;
    assert!(
        c.wait_for_text("^[[200~/tmp/a.png[2[201~01~rm^[[201~", PATIENCE)
            .await,
        "the bracketed paste did not arrive as one paste"
    );

    // Off again: the same paste arrives bare.
    c.input(b"\x03").await;
    c.input(b"printf '\\033[?2004l'; echo OFF_$((6*7)); cat -v\n")
        .await;
    assert!(c.wait_for_text("OFF_42", PATIENCE).await);
    c.forget_seen();
    c.send(&ClientWire::Paste {
        bytes: b"PLAIN_$((6*7))".to_vec(),
    })
    .await;
    c.input(b"\n").await;
    assert!(c.wait_for_text("PLAIN_$((6*7))", PATIENCE).await);
    let screen = c.screen(c.default_stream()).unwrap().lines.join("\n");
    let last = screen.lines().rfind(|l| l.contains("PLAIN_")).unwrap();
    assert!(
        !last.contains("^[[200~"),
        "markers on a program that never asked: {last}"
    );
}

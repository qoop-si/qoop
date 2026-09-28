//! Image passthrough gates: a real child emitting Kitty envelopes, end to end.
//!
//! The child prints pictures the way an agent does; the daemon must capture them
//! beside the grid, keep the neighbouring text intact, and remember the newest
//! one for a client that subscribes late.

use std::time::{Duration, Instant};

use qoro_testkit::{PATIENCE, TestDaemon};

/// A Kitty envelope from a real child is captured verbatim and stored on the
/// handle, while the text around it still reaches the screen.
#[tokio::test]
async fn a_kitty_envelope_from_a_child_is_captured_beside_the_grid() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("img").await;
    let stream = c.default_stream();
    // dash printf interprets octal escapes in the format string: `\033` is ESC,
    // so this prints BEFORE, a Kitty envelope, then AFTER.
    c.input(b"printf 'BEFORE\\n\\033_Gf=100,a=T;Zm9v\\033\\\\\\nAFTER\\n'\n")
        .await;
    assert!(
        c.wait_for_text("AFTER", PATIENCE).await,
        "the shell never answered"
    );
    let image = wait_for_image(&d, stream).await.expect("no image captured");
    assert_eq!(image.id, 1, "the first envelope is the first image");
    assert_eq!(image.protocol, qoro_runtime::daemon::ImageProtocol::Kitty);
    assert!(
        image.bytes.starts_with(b"\x1b_Gf=100,a=T;"),
        "envelope opening lost: {:?}",
        String::from_utf8_lossy(&image.bytes)
    );
    assert!(
        image.bytes.ends_with(b"\x1b\\"),
        "envelope terminator lost: {:?}",
        String::from_utf8_lossy(&image.bytes)
    );
    // The grid never saw the envelope as output, and the text around it is
    // intact. (The typed command line itself is echoed, payload included, so
    // the leak direction is covered by the sniffer and engine unit tests
    // instead: they feed the envelope with no echo in the way.)
    let screen = c.screen(stream).expect("a screen").lines.join("\n");
    assert!(
        screen.contains("BEFORE") && screen.contains("AFTER"),
        "neighbouring text did not survive: {screen}"
    );
}

/// The handle keeps the newest image only: a second envelope replaces the first.
#[tokio::test]
async fn a_second_envelope_replaces_the_first() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("img").await;
    let stream = c.default_stream();
    c.input(b"printf '\\033_Ga=T;eA==\\033\\\\\\n'\n").await;
    let first = wait_for_image(&d, stream).await.expect("no first image");
    c.input(b"printf '\\033_Ga=T;eQ==\\033\\\\\\n'\n").await;
    let deadline = Instant::now() + PATIENCE;
    let second = loop {
        let got = last_image(&d, stream);
        if let Some(img) = got
            && img.id != first.id
        {
            break img;
        }
        if Instant::now() > deadline {
            panic!("the second envelope never replaced the first");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(second.id, first.id + 1);
    assert!(second.bytes.ends_with(b"eQ==\x1b\\"));
}

/// A sixel envelope from a real child is captured beside the grid like the
/// other two protocols, while the text around it still reaches the screen.
#[tokio::test]
async fn a_sixel_envelope_from_a_child_is_captured_beside_the_grid() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("img").await;
    let stream = c.default_stream();
    c.input(b"printf 'BEFORE\\n\\033P1;1;10;10q#0;2;0;0;0$-\\033\\\\\\nAFTER\\n'\n")
        .await;
    assert!(
        c.wait_for_text("AFTER", PATIENCE).await,
        "the shell never answered"
    );
    let image = wait_for_image(&d, stream).await.expect("no image captured");
    assert_eq!(image.protocol, qoro_runtime::daemon::ImageProtocol::Sixel);
    assert!(
        image.bytes.starts_with(b"\x1bP1;1;10;10q"),
        "envelope opening lost: {:?}",
        String::from_utf8_lossy(&image.bytes)
    );
    assert!(
        image.bytes.ends_with(b"\x1b\\"),
        "envelope terminator lost: {:?}",
        String::from_utf8_lossy(&image.bytes)
    );
    let screen = c.screen(stream).expect("a screen").lines.join("\n");
    assert!(
        screen.contains("BEFORE") && screen.contains("AFTER"),
        "neighbouring text did not survive: {screen}"
    );
}

fn last_image(d: &TestDaemon, stream: u32) -> Option<qoro_runtime::daemon::StoredImage> {
    d.terminals()
        .get(stream)
        .and_then(|h| h.last_image.lock().unwrap().clone())
}

async fn wait_for_image(d: &TestDaemon, stream: u32) -> Option<qoro_runtime::daemon::StoredImage> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(img) = last_image(d, stream) {
            return Some(img);
        }
        if Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

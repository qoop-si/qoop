//! A program's links can never make its terminal unreachable.
//!
//! Every render carries the whole screen, and a snapshot larger than the reassembly bound
//! kills the client that receives it and every client that attaches after. A program
//! chooses its own `OSC 8` targets, so before the snapshot interned and bounded them, one
//! `perl` line that linked each cell to a four kilobyte address made a session nobody could
//! attach to: `stream 1 reassembly exceeded 8388608 bytes`, on every attempt.

use qoop_testkit::{PATIENCE, TestDaemon};

/// The reproduction, against a real daemon: cells alternately linked to two 4 KB
/// addresses, then a fresh attach, which has to get a screen and a small one.
#[tokio::test]
async fn a_screen_full_of_long_links_still_attaches_and_stays_small() {
    let d = TestDaemon::start().await;
    let mut first = d.attach("first").await;
    // A wide window: more cells, more links, well past the bound.
    first.resize(200, 60).await;
    first
        .input(
            b"perl -e 'for $i (1..3000){ $u=\"https://e.example/\".\"a\"x3990 .($i%2); \
              print \"\\e]8;;$u\\e\\\\x\" }'; echo; echo LINKS_DONE\n",
        )
        .await;
    assert!(
        first.wait_for_text("LINKS_DONE", PATIENCE * 4).await,
        "the client that watched it happen stopped hearing its terminal"
    );

    let mut later = d.attach("later").await;
    let stream = later.default_stream();
    later.input(b"echo STILL_HERE\n").await;
    let screen = later
        .wait_for(
            |s| s.lines.iter().any(|l| l.contains("STILL_HERE")),
            PATIENCE * 4,
        )
        .await
        .expect("a client attaching afterwards gets a screen");
    let bytes = serde_json::to_vec(&screen).expect("encode").len();
    assert!(
        bytes < 256 * 1024,
        "a screen of {bytes} bytes for terminal {stream}"
    );
}

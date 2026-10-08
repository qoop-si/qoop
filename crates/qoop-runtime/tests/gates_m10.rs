//! Milestone 10 gates: a phone attaches and nothing on the desk changes.
//!
//! The failure this rules out is the one that makes a mobile client unusable in practice:
//! somebody opens their workspace on a phone during a meeting, and every terminal on their
//! desktop reflows to 40 columns.

use std::time::Duration;

use qoop_testkit::{PATIENCE, TestDaemon};

/// G10.1: a mobile-sized client attaching never changes any pty winsize.
///
/// Asserted as an absence after a fresh marker proves the screen has been updated since,
/// because the earlier `stty size` output is still on the screen and "the old size is
/// present" is true whichever way the size went.
#[tokio::test]
async fn g10_1_a_phone_sized_client_never_changes_a_terminals_size() {
    let d = TestDaemon::start().await;
    let desk = d.terminals().streams()[0];

    let mut desktop = d.attach("desktop").await;
    desktop.resize(180, 50).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    desktop.input_to(desk, b"stty size\n").await;
    assert!(desktop.wait_for_text_on(desk, "50 180", PATIENCE).await);

    // Several phone shaped clients, attaching and resizing as a phone would.
    let mut phones = Vec::new();
    for (i, (cols, rows)) in [(40u16, 20u16), (32, 16), (44, 24)].iter().enumerate() {
        let mut phone = d.attach(&format!("phone{i}")).await;
        phone.resize(*cols, *rows).await;
        phones.push(phone);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    desktop.forget_seen();
    desktop
        .input_to(desk, b"echo AFTER_$((6*7)); stty size\n")
        .await;
    assert!(desktop.wait_for_text_on(desk, "AFTER_42", PATIENCE).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    desktop
        .wait_for_text_on(desk, "never matches", Duration::from_millis(200))
        .await;
    let screen = desktop.screen(desk).expect("a screen").lines.join("\n");
    for phone_size in ["20 40", "16 32", "24 44"] {
        assert!(
            !screen.contains(phone_size),
            "a phone resized the terminal to {phone_size}:\n{screen}"
        );
    }
}

/// And a phone that attaches while nobody owns the terminal does take it, because an
/// unowned terminal has no geometry to protect.
///
/// Worth pinning, because the obvious way to satisfy the gate above is to refuse every
/// resize from a small client, which would make a phone-only workspace unusable.
#[tokio::test]
async fn a_phone_can_own_a_terminal_that_nobody_else_has_claimed() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut phone = d.attach("phone").await;
    phone.resize(40, 20).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    phone.input_to(terminal, b"stty size\n").await;
    assert!(
        phone.wait_for_text_on(terminal, "20 40", PATIENCE).await,
        "a phone could not size a terminal nobody else owns, so a phone only workspace \
         would be unusable"
    );
}

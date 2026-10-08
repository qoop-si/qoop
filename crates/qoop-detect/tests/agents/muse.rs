//! Muse Code, screens built from the strings in its 1.4.3 release.

use super::*;
use AgentRuntimeState::Blocked;

#[test]
fn the_trust_prompt_is_a_setup_gate() {
    let screen = [
        "Do you trust this workspace?",
        "Workspace: ~/project",
        "Trust target: ~/project",
        "Only trust this workspace when you trust its contents.",
        "1. Trust and continue",
        "2. Quit",
        "Use Up/Down or 1/2, then Enter. Esc quits.",
    ];
    assert_rule("muse", &screen, "trust_workspace", Blocked);
    assert!(verdict("muse", &screen).is_some_and(|v| v.setup_gate));
    let quoted = [
        "It first asks:",
        "Do you trust this workspace?",
        "Use Up/Down or 1/2, then Enter. Esc quits.",
        "> ",
    ];
    assert_not_rule("muse", &quoted, "trust_workspace");
    assert_no_blocker("muse", &quoted);
}

#[test]
fn the_hook_review_is_a_setup_gate() {
    let screen = [
        "Review hooks",
        "Hooks run commands on your machine when their event matches.",
        "> Trust all and continue",
        "  Continue without trusting",
        "Up/Down to choose, Enter to confirm. Esc continues without trusting.",
    ];
    assert_rule("muse", &screen, "review_hooks", Blocked);
    assert!(verdict("muse", &screen).is_some_and(|v| v.setup_gate));
    let quoted = [
        "The hook review shows:",
        "> Trust all and continue",
        "Up/Down to choose, Enter to confirm. Esc continues without trusting.",
        "> ",
    ];
    assert_not_rule("muse", &quoted, "review_hooks");
    assert_no_blocker("muse", &quoted);
}

#[test]
fn pickers_leave_the_reading_alone() {
    assert_skip(
        "muse",
        &[
            "Resume a previous session",
            "> fix the build",
            "esc clear  ctrl+t transcript  ctrl+e expand  ↑↓ browse  pgup/pgdn page",
        ],
        "picker_open",
    );
    assert_skip(
        "muse",
        &[
            "Rewind conversation",
            "Creates a new conversation branch. Your files stay unchanged.",
            "> 3. fix the build",
        ],
        "picker_open",
    );
    assert_not_rule(
        "muse",
        &["Use /rewind to open Rewind conversation.", "> "],
        "picker_open",
    );
}

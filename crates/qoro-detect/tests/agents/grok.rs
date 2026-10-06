//! `grok`: Grok Build (xai-org/grok-build) and grok-cli (superagent-ai/grok-cli), screens
//! built from each one's source.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

/// Grok Build's composer box at rest.
const BUILD_COMPOSER: &[&str] = &[
    "╭──────────────────────────────────────────╮",
    "│ ❯ Build anything                         │",
    "╰──────────────────────────────────────────╯",
];

/// grok-cli's composer at rest.
const CLI_COMPOSER: &[&str] = &["  Message Grok...", "  grok-4 · ~/project"];

#[test]
fn the_action_required_title_is_a_blocker() {
    assert_rule_titled(
        "grok",
        BUILD_COMPOSER,
        "⚠ Action Required - my session - grok",
        "title_action_required",
        Blocked,
    );
    assert_not_rule_titled(
        "grok",
        &[
            "  Allow `rm`?",
            "1 (●) Yes",
            "2 (○) No, reject (type to add feedback)",
        ],
        "my session - grok",
        "title_action_required",
    );
}

#[test]
fn the_trust_question_is_a_setup_gate() {
    let screen = [
        "            GROK BUILD",
        "  Do you trust the contents of this directory?",
        "  ~/project",
        "  Grok Build may run or modify contents in this directory,",
        "  posing security risks.",
        "  y Yes, proceed",
        "  n No, quit",
    ];
    assert_rule("grok", &screen, "trust_directory", Blocked);
    assert!(verdict("grok", &screen).is_some_and(|v| v.setup_gate));
    let mut quoted = vec![
        "It asked:",
        "  Do you trust the contents of this directory?",
        "  Grok Build may run or modify contents in this directory,",
    ];
    quoted.extend(BUILD_COMPOSER);
    assert_not_rule("grok", &quoted, "trust_directory");
    assert_no_blocker("grok", &quoted);
}

#[test]
fn grok_cli_payment_approval_is_a_blocker() {
    let screen = [
        "┃ Payment required",
        "┃ https://api.example.com/data",
        "┃ Security: verified",
        "┃ Price: $0.01",
        "┃ > Approve payment",
        "┃   Reject",
        "┃ ↑↓ select   enter confirm   esc reject",
    ];
    assert_rule("grok", &screen, "payment_approval", Blocked);
    let mut quoted = vec![
        "It shows:",
        "Payment required",
        "↑↓ select   enter confirm   esc reject",
    ];
    quoted.extend(CLI_COMPOSER);
    assert_not_rule("grok", &quoted, "payment_approval");
    assert_no_blocker("grok", &quoted);
}

#[test]
fn grok_build_turn_status_is_working() {
    let mut screen = vec!["⠧ Run command 0.2s              1m20s ⇣12k [stop]"];
    screen.extend(BUILD_COMPOSER);
    assert_rule("grok", &screen, "turn_status", Working);
    assert_not_rule("grok", BUILD_COMPOSER, "turn_status");
}

#[test]
fn grok_build_spinner_title_is_working() {
    assert_rule_titled(
        "grok",
        &[""],
        "⠋ - Thinking - my session - grok",
        "title_spinner",
        Working,
    );
    assert_not_rule_titled("grok", &[""], "my session - grok", "title_spinner");
}

#[test]
fn grok_cli_working_prompt_is_working() {
    assert_rule(
        "grok",
        &["  Queue a follow-up... (esc to interrupt)", "  grok-4"],
        "grok_cli_working",
        Working,
    );
    assert_rule(
        "grok",
        &["  Planning next moves", "  Message Grok..."],
        "grok_cli_working",
        Working,
    );
    assert_not_rule("grok", CLI_COMPOSER, "grok_cli_working");
}

#[test]
fn both_composers_are_idle() {
    assert_rule("grok", BUILD_COMPOSER, "composer_placeholder", Idle);
    assert_rule("grok", CLI_COMPOSER, "composer_placeholder", Idle);
    assert_not_rule(
        "grok",
        &["│ ❯ Build anything you like, it said      │"],
        "composer_placeholder",
    );
}

/// A live trust question under a previous session's composer.
#[test]
fn a_live_gate_under_scrollback_is_still_a_gate() {
    let mut screen = BUILD_COMPOSER.to_vec();
    screen.extend([
        "$ cd ../other && grok",
        "  Do you trust the contents of this directory?",
        "  Grok Build may run or modify contents in this directory,",
        "  y Yes, proceed",
        "  n No, quit",
    ]);
    assert_rule("grok", &screen, "trust_directory", Blocked);
}

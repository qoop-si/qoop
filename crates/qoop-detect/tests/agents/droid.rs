//! Droid CLI, screens built from the interface catalog in its 0.234.0 binary.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const INPUT: &[&str] = &[
    "╭──────────────────────────────────────────╮",
    "│ >                                        │",
    "╰──────────────────────────────────────────╯",
    "  Press ? for help",
    "  Auto (Off)  shift+tab to cycle modes (auto/spec), ctrl+L for autonomy",
];

fn with_input(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(INPUT).copied().collect()
}

#[test]
fn a_tool_confirmation_is_a_blocker() {
    let screen = [
        "  Execute: rm -rf build",
        "  > Yes, allow",
        "    Yes, and always allow Medium",
        "    No, cancel",
        "⠋ Waiting for tool confirmation...",
    ];
    assert_rule("droid", &screen, "tool_confirmation", Blocked);
    let quoted = with_input(&["  It offers:", "  Yes, allow", "  No, cancel"]);
    assert_not_rule("droid", &quoted, "tool_confirmation");
    assert_no_blocker("droid", &quoted);
    // Whether the input stays under a live confirmation is not sourced: if it does, the
    // confirmation is still read.
    let mut with_input_below = screen.to_vec();
    with_input_below.extend(INPUT);
    assert_rule("droid", &with_input_below, "tool_confirmation", Blocked);
}

#[test]
fn droid_shield_is_a_blocker() {
    let screen = [
        "  Droid-Shield approval required",
        "  ✕ Proceed anyway",
        "    Proceed & report false positive",
        "    Cancel",
    ];
    assert_rule("droid", &screen, "droid_shield", Blocked);
    let quoted = with_input(&[
        "  Droid-Shield approval required",
        "  ✕ Proceed anyway",
        "    Proceed & report false positive",
        "    Cancel",
    ]);
    assert_not_rule("droid", &quoted, "droid_shield");
    assert_no_blocker("droid", &quoted);
}

#[test]
fn a_question_is_a_blocker() {
    let screen = [
        "  ASK USER",
        "  Which database?",
        "  > Postgres",
        "    Or type your own answer...",
        "  ↑/↓ Navigate • Enter Select • Ctrl+R Refresh • ESC stop agent",
    ];
    assert_rule("droid", &screen, "ask_user", Blocked);
    let quoted = with_input(&["  The footer reads ↑/↓ Navigate • Enter Select • ESC stop agent"]);
    assert_not_rule("droid", &quoted, "ask_user");
    assert_no_blocker("droid", &quoted);
}

#[test]
fn folder_trust_and_login_are_setup_gates() {
    let trust = [
        "Trust this folder?",
        "Droid will read, edit, and run files in this folder.",
        "> 1. Trust this folder",
        "  2. Exit without trusting",
        "Enter to confirm · Esc to exit",
    ];
    assert_rule("droid", &trust, "folder_trust", Blocked);
    assert!(verdict("droid", &trust).is_some_and(|v| v.setup_gate));
    let quoted = with_input(&[
        "  It asks:",
        "  Trust this folder?",
        "  Enter to confirm · Esc to exit",
    ]);
    assert_not_rule("droid", &quoted, "folder_trust");
    assert_no_blocker("droid", &quoted);

    let login = [
        "Please login with your Factory account to continue.",
        "> Login with Factory account",
        "  Continue with API key",
    ];
    assert_rule("droid", &login, "login", Blocked);
    let quoted = with_input(&[
        "  The login screen offers:",
        "  Login with Factory account",
        "  Continue with API key",
    ]);
    assert_not_rule("droid", &quoted, "login");
    assert_no_blocker("droid", &quoted);
}

#[test]
fn the_spinner_label_is_working() {
    assert_rule(
        "droid",
        &with_input(&["⠹ Invoking tools..."]),
        "spinner_label",
        Working,
    );
    assert_rule(
        "droid",
        &["│ > Enter to steer · Ctrl+Enter to queue │"],
        "spinner_label",
        Working,
    );
    assert_not_rule(
        "droid",
        &with_input(&["  It was Thinking... about the parser."]),
        "spinner_label",
    );
}

#[test]
fn the_help_hint_is_idle() {
    assert_rule("droid", INPUT, "help_hint", Idle);
    assert_not_rule(
        "droid",
        &["  Press ? for help with commands, it said.", "│ > │"],
        "help_hint",
    );
}

#[test]
fn pickers_leave_the_reading_alone() {
    assert_skip(
        "droid",
        &[
            "  Select a model",
            "  > gpt-5",
            "  Use ↑↓ to navigate, Tab/Enter to select, Esc to cancel",
        ],
        "picker_open",
    );
    assert_not_rule("droid", INPUT, "picker_open");
}

/// Live gates under a previous session's input and its help hint.
#[test]
fn a_live_gate_under_scrollback_is_still_a_gate() {
    let mut trust = INPUT.to_vec();
    trust.extend([
        "$ cd ../other && droid",
        "Trust this folder?",
        "> 1. Trust this folder",
        "  2. Exit without trusting",
        "Enter to confirm · Esc to exit",
    ]);
    assert_rule("droid", &trust, "folder_trust", Blocked);
    let mut login = INPUT.to_vec();
    login.extend([
        "$ droid",
        "> Login with Factory account",
        "  Continue with API key",
    ]);
    assert_rule("droid", &login, "login", Blocked);
    let mut shield = INPUT.to_vec();
    shield.extend([
        "  Droid-Shield approval required",
        "  ✕ Proceed anyway",
        "    Proceed & report false positive",
    ]);
    assert_rule("droid", &shield, "droid_shield", Blocked);
}

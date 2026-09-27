//! Qoder CLI, screens built from the strings in @qoder-ai/qodercli 1.1.65.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "╭──────────────────────────────────────────────╮",
    "│ >   Type your message or @path/to/file       │",
    "╰──────────────────────────────────────────────╯",
    "  ctx 12% used",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn a_permission_dialog_is_a_blocker() {
    let screen = [
        "Allow this command to run? Redirection detected.",
        "  ls > out.txt",
        " ❯ 1. Allow once",
        "      Run this command for the current request only.",
        "   2. Reject and type something",
        "      Deny and send a message back to the model.",
        "   3. No",
        "      Deny this command without feedback.",
        "⠏ Waiting for user confirmation...",
    ];
    assert_rule("qodercli", &screen, "permission_dialog", Blocked);
    assert_rule(
        "qodercli",
        &[
            "Qoder has written up a plan and is ready to execute. Would you like to proceed?",
            " ❯ 1. Yes",
            "   2. No",
        ],
        "permission_dialog",
        Blocked,
    );
}

#[test]
fn a_permission_dialog_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&["It will ask:", "Apply this change?", " ❯ 1. Allow once"]);
    assert_not_rule("qodercli", &screen, "permission_dialog");
    assert_no_blocker("qodercli", &screen);
}

#[test]
fn the_title_reads_each_state() {
    assert_rule_titled(
        "qodercli",
        &[""],
        "▲ fix the build | Action Required",
        "title_action_required",
        Blocked,
    );
    assert_rule_titled(
        "qodercli",
        &[""],
        "✦ fix the build | Working",
        "title_working",
        Working,
    );
    assert_rule_titled(
        "qodercli",
        &[""],
        "◇ Qoder CLI | Ready",
        "title_ready",
        Idle,
    );
    assert_not_rule_titled(
        "qodercli",
        &[""],
        "Qoder CLI (fix the build)",
        "title_action_required",
    );
    assert_not_rule_titled("qodercli", &[""], "◇ Qoder CLI | Ready", "title_working");
    assert_not_rule_titled("qodercli", &[""], "✦ fix | Working", "title_ready");
}

#[test]
fn the_trust_gate_is_a_setup_gate() {
    let screen = [
        "Do you trust the files in this folder?",
        "Please confirm this is your own project or from a trusted source.",
        " ❯ 1. Trust folder",
        "   2. Don't trust and exit",
    ];
    assert_rule("qodercli", &screen, "trust_folder", Blocked);
    assert!(verdict("qodercli", &screen).is_some_and(|v| v.setup_gate));
    let quoted = with_composer(&[
        "It asks:",
        "Do you trust the files in this folder?",
        " ❯ 1. Trust folder",
        "   2. Don't trust and exit",
    ]);
    assert_not_rule("qodercli", &quoted, "trust_folder");
    assert_no_blocker("qodercli", &quoted);
}

#[test]
fn the_loading_row_is_working() {
    assert_rule(
        "qodercli",
        &with_composer(&["⠹ Generating... (esc to cancel, 12s)"]),
        "esc_to_cancel",
        Working,
    );
    assert_not_rule(
        "qodercli",
        &with_composer(&["Press esc to cancel a request."]),
        "esc_to_cancel",
    );
}

#[test]
fn the_composer_is_idle() {
    assert_rule("qodercli", COMPOSER, "composer_placeholder", Idle);
    assert_rule(
        "qodercli",
        &["│ !   Type your shell command                  │"],
        "composer_placeholder",
        Idle,
    );
    assert_not_rule(
        "qodercli",
        &["│ > fix the build                              │"],
        "composer_placeholder",
    );
}

#[test]
fn the_copy_dialog_leaves_the_reading_alone() {
    assert_skip(
        "qodercli",
        &[
            "Copy",
            "Select content to copy.",
            "Enter copy · W write to file · Esc cancel",
        ],
        "copy_dialog",
    );
    assert_not_rule("qodercli", COMPOSER, "copy_dialog");
}

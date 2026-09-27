//! Qwen Code, screens built from `packages/cli/src/ui` and its snapshots.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "╭──────────────────────────────────────────────╮",
    "│ >   Type your message or @path/to/file       │",
    "╰──────────────────────────────────────────────╯",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn an_exec_confirmation_is_a_blocker() {
    let screen = [
        "│ ╭──────────────────────────────────────────╮ │",
        "│ │ rm -rf build                             │ │",
        "│ ╰──────────────────────────────────────────╯ │",
        "│ Allow execution of: 'rm'?                    │",
        "│                                              │",
        "│ › 1. Yes, allow once                         │",
        "│   2. Always allow in this project            │",
        "│   3. Always allow for this user              │",
        "│   4. No, suggest changes (esc)               │",
        "⠏ Waiting for user confirmation...",
    ];
    assert_rule("qwen", &screen, "tool_confirmation", Blocked);
}

#[test]
fn the_shell_command_dialog_and_plan_exit_are_blockers() {
    assert_rule(
        "qwen",
        &[
            "Shell Command Execution",
            "A custom command wants to run the following shell commands:",
            "Do you want to proceed?",
            "› 1. Yes, allow once",
            "  2. Always allow in this project",
            "  3. Always allow for this user",
            "  4. No (esc)",
        ],
        "tool_confirmation",
        Blocked,
    );
    assert_rule(
        "qwen",
        &[
            "Refactor the parser",
            "› 1. Yes, restore previous mode (default)",
            "  2. Yes, and auto-accept edits",
            "  3. Yes, and manually approve edits",
            "  4. No, keep planning (esc)",
        ],
        "tool_confirmation",
        Blocked,
    );
}

#[test]
fn a_confirmation_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "✦ The dialog looks like this:",
        "  Apply this change?",
        "  › 1. Yes, allow once",
        "    2. No, suggest changes (esc)",
    ]);
    assert_not_rule("qwen", &screen, "tool_confirmation");
    assert_no_blocker("qwen", &screen);
}

#[test]
fn the_waiting_title_is_a_blocker() {
    assert_rule_titled(
        "qwen",
        &[""],
        "✳\u{FE0E} Qwen - project",
        "title_waiting_for_confirmation",
        Blocked,
    );
    assert_not_rule_titled(
        "qwen",
        &[""],
        "Qwen - project",
        "title_waiting_for_confirmation",
    );
}

#[test]
fn the_trust_dialog_is_a_setup_gate() {
    let screen = [
        "╭──────────────────────────────────────────────╮",
        "│ Do you trust this folder?                    │",
        "│                                              │",
        "│ Trusting a folder allows Qwen Code to execute│",
        "│ commands it suggests.                        │",
        "│                                              │",
        "│ › 1. Trust folder (project)                  │",
        "│   2. Trust parent folder (code)              │",
        "│   3. Don't trust (esc)                       │",
        "╰──────────────────────────────────────────────╯",
    ];
    assert_rule("qwen", &screen, "trust_folder", Blocked);
    assert!(verdict("qwen", &screen).is_some_and(|v| v.setup_gate));
    let quoted = with_composer(&[
        "✦ It first asks:",
        "  Do you trust this folder?",
        "  › 1. Trust folder (project)",
        "    3. Don't trust (esc)",
    ]);
    assert_not_rule("qwen", &quoted, "trust_folder");
    assert_no_blocker("qwen", &quoted);
}

#[test]
fn the_loading_line_is_working() {
    assert_rule(
        "qwen",
        &with_composer(&["  ⠼ Painting the serifs back on... (5s · ↓ 120 tokens · esc to cancel)"]),
        "esc_to_cancel",
        Working,
    );
    assert_rule(
        "qwen",
        &with_composer(&["  ⠼ (12s · esc to cancel)"]),
        "esc_to_cancel",
        Working,
    );
    assert_not_rule(
        "qwen",
        &with_composer(&["✦ Press esc to cancel a running request."]),
        "esc_to_cancel",
    );
}

#[test]
fn the_responding_title_is_working() {
    assert_rule_titled(
        "qwen",
        &[""],
        "◐\u{FE0E} Qwen - project",
        "title_responding",
        Working,
    );
    assert_not_rule_titled("qwen", &[""], "Qwen - project", "title_responding");
}

#[test]
fn the_composer_is_idle() {
    assert_rule("qwen", COMPOSER, "composer_placeholder", Idle);
    assert_rule(
        "qwen",
        &[">   Press 'i' for INSERT mode and 'Esc' for NORMAL mode."],
        "composer_placeholder",
        Idle,
    );
    assert_not_rule(
        "qwen",
        &["│ > fix the build                              │"],
        "composer_placeholder",
    );
}

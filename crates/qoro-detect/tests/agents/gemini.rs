//! Gemini CLI, screens built from `packages/cli/src/ui` and its vitest snapshots.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "╭──────────────────────────────────────────────╮",
    "│ >   Type your message or @path/to/file       │",
    "╰──────────────────────────────────────────────╯",
    "~/project (main)        no sandbox        gemini-2.5-pro",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn a_shell_confirmation_is_a_blocker() {
    let screen = [
        "╭──────────────────────────────────────────────╮",
        "│ ? Shell ls -la                               │",
        "│ ╭──────────────────────────────────────────╮ │",
        "│ │ ls -la                                   │ │",
        "│ ╰──────────────────────────────────────────╯ │",
        "│ Allow execution of [Shell]?                  │",
        "│                                              │",
        "│ ● 1. Allow once                              │",
        "│   2. Allow for this session                  │",
        "│   3. No, suggest changes (esc)               │",
        "╰──────────────────────────────────────────────╯",
        "⠏ Waiting for user confirmation...",
    ];
    assert_rule("gemini", &screen, "tool_confirmation", Blocked);
}

#[test]
fn an_edit_and_an_mcp_confirmation_are_blockers() {
    assert_rule(
        "gemini",
        &[
            "Apply this change?",
            "",
            "● 1. Allow once",
            "  2. Allow for this session",
            "  3. Allow for this file in all future sessions ~/.gemini/policies/auto-saved.toml",
            "  4. Modify with external editor",
            "  5. No, suggest changes (esc)",
        ],
        "tool_confirmation",
        Blocked,
    );
    assert_rule(
        "gemini",
        &[
            "MCP Server: testserver",
            "Tool: testtool",
            "Allow execution of MCP tool \"testtool\" from server \"testserver\"?",
            "",
            "● 1. Allow once",
            "  2. Allow tool for this session",
            "  3. Allow all server tools for this session",
            "  4. No, suggest changes (esc)",
        ],
        "tool_confirmation",
        Blocked,
    );
}

#[test]
fn a_confirmation_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "✦ Gemini will show this before running it:",
        "  Do you want to proceed?",
        "  ● 1. Allow once",
        "    2. No, suggest changes (esc)",
    ]);
    assert_not_rule("gemini", &screen, "tool_confirmation");
    assert_no_blocker("gemini", &screen);
}

#[test]
fn the_action_required_title_is_a_blocker() {
    assert_rule_titled(
        "gemini",
        &[""],
        "✋  Action Required (project)",
        "title_action_required",
        Blocked,
    );
    assert_not_rule_titled(
        "gemini",
        &[""],
        "Gemini CLI (project)",
        "title_action_required",
    );
}

#[test]
fn the_trust_and_sign_in_dialogs_are_setup_gates() {
    let trust = [
        "╭──────────────────────────────────────────────╮",
        "│ Do you trust the files in this folder?       │",
        "│                                              │",
        "│ Trusting a folder allows Gemini CLI to load  │",
        "│ its local configurations.                    │",
        "│                                              │",
        "│ ● 1. Trust folder (project)                  │",
        "│   2. Trust parent folder (code)              │",
        "│   3. Don't trust                             │",
        "╰──────────────────────────────────────────────╯",
    ];
    assert_rule("gemini", &trust, "trust_folder", Blocked);
    assert!(verdict("gemini", &trust).is_some_and(|v| v.setup_gate));
    let quoted = with_composer(&[
        "✦ On first start Gemini shows:",
        "  Do you trust the files in this folder?",
        "  ● 1. Trust folder (project)",
        "    3. Don't trust",
    ]);
    assert_not_rule("gemini", &quoted, "trust_folder");
    assert_no_blocker("gemini", &quoted);

    let auth = [
        "╭──────────────────────────────────────────────╮",
        "│ ? Get started                                │",
        "│                                              │",
        "│ How would you like to authenticate for this project? │",
        "╰──────────────────────────────────────────────╯",
    ];
    assert_rule("gemini", &auth, "auth_dialog", Blocked);
    let quoted = with_composer(&[
        "✦ Signing in looks like this:",
        "  ? Get started",
        "  How would you like to authenticate for this project?",
    ]);
    assert_not_rule("gemini", &quoted, "auth_dialog");
    assert_no_blocker("gemini", &quoted);
}

#[test]
fn the_loading_line_is_working() {
    assert_rule(
        "gemini",
        &with_composer(&["⠼ Thinking... (esc to cancel, 5s)"]),
        "esc_to_cancel",
        Working,
    );
    assert_rule(
        "gemini",
        &with_composer(&["⠼ Reading the parser (esc to cancel, 1m 5s)"]),
        "esc_to_cancel",
        Working,
    );
    assert_not_rule(
        "gemini",
        &with_composer(&["✦ Press esc to cancel a running request."]),
        "esc_to_cancel",
    );
}

#[test]
fn the_working_title_is_working() {
    assert_rule_titled(
        "gemini",
        &[""],
        "✦  Working… (project)",
        "title_working",
        Working,
    );
    assert_rule_titled(
        "gemini",
        &[""],
        "⏲  Working… (project)",
        "title_working",
        Working,
    );
    assert_not_rule_titled("gemini", &[""], "◇  Ready (project)", "title_working");
}

#[test]
fn the_composer_is_idle() {
    assert_rule("gemini", COMPOSER, "composer_placeholder", Idle);
    assert_rule(
        "gemini",
        &["*   Type your message or @path/to/file"],
        "composer_placeholder",
        Idle,
    );
    assert_not_rule(
        "gemini",
        &["│ > fix the build                              │"],
        "composer_placeholder",
    );
}

#[test]
fn the_ready_title_is_idle() {
    assert_rule_titled("gemini", &[""], "◇  Ready (project)", "title_ready", Idle);
    assert_not_rule_titled("gemini", &[""], "Gemini CLI (project)", "title_ready");
}

#[test]
fn dialogs_the_person_opened_leave_the_reading_alone() {
    assert_skip(
        "gemini",
        &[
            "│ Select Model │",
            "│ ● 1. Auto    │",
            "│ (Press Esc to close) │",
        ],
        "dialog_open",
    );
    assert_skip(
        "gemini",
        &["Use arrow keys to navigate, Enter to confirm, Esc to cancel."],
        "dialog_open",
    );
    assert_not_rule("gemini", COMPOSER, "dialog_open");
}

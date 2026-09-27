//! Cline CLI, screens built from the views and components in `apps/cli/src/tui`.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const STATUS_BAR: &[&str] = &[
    " claude-sonnet-4 · 12k/200k                         ○ Plan  ● Act (Tab)",
    " ~/project | 2 files +10 -3",
    " Auto-approve all disabled (Shift+Tab)",
];

fn screen(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(STATUS_BAR).copied().collect()
}

fn idle_with(above: &[&'static str]) -> Vec<&'static str> {
    let mut lines: Vec<&str> = above.to_vec();
    lines.push(" Ask anything...");
    lines.extend(STATUS_BAR);
    lines
}

#[test]
fn the_approval_panel_is_a_blocker() {
    let panel = screen(&[
        " Cline needs permission",
        "",
        " Approve tool call?",
        "",
        " execute_command",
        "",
        " {\"command\":\"rm -rf build\"}",
        "",
        "  [y] Approve   [n] Deny ",
    ]);
    assert_rule("cline", &panel, "tool_approval", Blocked);
}

#[test]
fn the_approval_panel_quoted_in_an_answer_is_not_a_blocker() {
    let quoted = idle_with(&[
        "Cline shows this panel:",
        " Approve tool call?",
        "  [y] Approve   [n] Deny ",
    ]);
    assert_not_rule("cline", &quoted, "tool_approval");
    assert_no_blocker("cline", &quoted);
}

#[test]
fn a_question_is_a_blocker() {
    let panel = screen(&[
        " Cline is asking a question",
        "",
        " Which database should I use?",
        " 1. Postgres",
        " 2. SQLite",
        " Type a response...",
    ]);
    assert_rule("cline", &panel, "ask_question", Blocked);
    let quoted = idle_with(&["The title reads:", " Cline is asking a question"]);
    assert_not_rule("cline", &quoted, "ask_question");
    assert_no_blocker("cline", &quoted);
}

#[test]
fn provider_onboarding_is_a_setup_gate() {
    let onboarding = [
        " Choose a provider",
        " Search providers...",
        " > Anthropic",
        "   OpenRouter",
        " Tab to switch fields, Enter to save, Esc to go back, Ctrl+C to exit",
    ];
    assert_rule("cline", &onboarding, "provider_onboarding", Blocked);
    assert!(verdict("cline", &onboarding).is_some_and(|v| v.setup_gate));
    assert_rule(
        "cline",
        &[" Enter to continue, R to recheck, Esc to go back, Ctrl+C to exit"],
        "provider_onboarding",
        Blocked,
    );
    assert_not_rule(
        "cline",
        &idle_with(&[
            "Onboarding says:",
            " Enter to save, Esc to go back, Ctrl+C to exit",
        ]),
        "provider_onboarding",
    );
}

#[test]
fn the_thinking_line_is_working() {
    assert_rule(
        "cline",
        &idle_with(&["⠹ Thinking... (esc to cancel)"]),
        "thinking_esc_to_cancel",
        Working,
    );
    assert_not_rule(
        "cline",
        &idle_with(&["Press Thinking... (esc to cancel) to stop."]),
        "thinking_esc_to_cancel",
    );
}

#[test]
fn queued_prompts_are_working() {
    assert_rule(
        "cline",
        &idle_with(&[
            " 1. also update the docs",
            " ↑/↓ navigate, Enter steer, Tab edit, Esc cancels turn",
        ]),
        "queued_prompts",
        Working,
    );
    assert_not_rule("cline", &idle_with(&[]), "queued_prompts");
}

#[test]
fn the_empty_input_bar_is_idle() {
    assert_rule("cline", &idle_with(&[]), "input_placeholder", Idle);
    assert_rule(
        "cline",
        &screen(&[" Plan something..."]),
        "input_placeholder",
        Idle,
    );
    assert_rule(
        "cline",
        &screen(&[" What can I do for you?"]),
        "input_placeholder",
        Idle,
    );
    assert_not_rule(
        "cline",
        &screen(&[" Ask anything... about the parser"]),
        "input_placeholder",
    );
}

#[test]
fn pickers_leave_the_reading_alone() {
    assert_skip(
        "cline",
        &[
            " Select a model",
            " > claude-sonnet-4",
            " ↑/↓ navigate, Enter to select, Esc to go back",
        ],
        "picker_open",
    );
    assert_skip(
        "cline",
        &[" Theme", " ↑/↓ preview, Enter to apply, Esc to cancel"],
        "picker_open",
    );
    assert_not_rule("cline", &idle_with(&[]), "picker_open");
}

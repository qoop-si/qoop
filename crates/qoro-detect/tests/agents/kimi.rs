//! Kimi Code CLI, screens built from `src/kimi_cli/ui/shell`.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const PROMPT: &[&str] = &[
    "✨ ",
    "~/project  main  ctrl-x: toggle mode | shift-tab: plan mode | ctrl-o: editor",
];

fn with_prompt(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(PROMPT).copied().collect()
}

#[test]
fn the_approval_panel_is_a_blocker() {
    let screen = [
        "╭─ approval ─────────────────────────────────────────────╮",
        "│ Shell is requesting approval to run command:           │",
        "│                                                        │",
        "│ rm -rf build                                           │",
        "│                                                        │",
        "│ → [1] Approve once                                     │",
        "│   [2] Approve for this session                         │",
        "│   [3] Reject                                           │",
        "│   [4] Reject, tell the model what to do instead        │",
        "│   ▲/▼ select  1/2/3/4 choose  ↵ confirm                │",
        "╰────────────────────────────────────────────────────────╯",
        "✨ Use ↑/↓ or 1/2/3, then press Enter to respond to the approval request.",
    ];
    assert_rule("kimi", &screen, "approval_panel", Blocked);
}

#[test]
fn the_approval_panel_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_prompt(&[
        "The panel looks like this:",
        "→ [1] Approve once",
        "  [3] Reject",
        "  ▲/▼ select  1/2/3/4 choose  ↵ confirm",
    ]);
    assert_not_rule("kimi", &screen, "approval_panel");
    assert_no_blocker("kimi", &screen);
}

#[test]
fn the_question_panel_is_a_blocker() {
    let screen = [
        "╭─ question ─────────────────────────────────────────────╮",
        "│ Which database should I use?                           │",
        "│ → Postgres                                             │",
        "│   Other                                                │",
        "│   ◄/► switch question  ▲/▼ select  ↵ submit  esc exit  │",
        "╰────────────────────────────────────────────────────────╯",
    ];
    assert_rule("kimi", &screen, "question_panel", Blocked);
    let quoted = with_prompt(&[
        "It asks a question and shows:",
        "  ◄/► switch question  ▲/▼ select  ↵ submit  esc exit",
    ]);
    assert_not_rule("kimi", &quoted, "question_panel");
    assert_no_blocker("kimi", &quoted);
}

#[test]
fn the_api_key_setup_is_a_setup_gate() {
    let screen = ["Welcome to Kimi Code CLI!", "Enter your API key: "];
    assert_rule("kimi", &screen, "api_key_setup", Blocked);
    assert!(verdict("kimi", &screen).is_some_and(|v| v.setup_gate));
    assert_not_rule(
        "kimi",
        &with_prompt(&["Run /login and you will see:", "Enter your API key: "]),
        "api_key_setup",
    );
}

#[test]
fn the_thinking_line_is_working() {
    assert_rule(
        "kimi",
        &with_prompt(&["Thinking..   3.2s · 120 tokens · 37 tok/s"]),
        "thinking_line",
        Working,
    );
    assert_not_rule(
        "kimi",
        &with_prompt(&["Thinking about it, the tokens are fine."]),
        "thinking_line",
    );
}

#[test]
fn the_spinner_line_is_working() {
    assert_rule(
        "kimi",
        &with_prompt(&["⠹ Composing... 4.1s · 88 tokens"]),
        "spinner_line",
        Working,
    );
    assert_not_rule(
        "kimi",
        &with_prompt(&["Composing... is what it shows while writing."]),
        "spinner_line",
    );
}

#[test]
fn the_prompt_toolbar_is_idle() {
    assert_rule("kimi", PROMPT, "prompt_toolbar", Idle);
    assert_not_rule(
        "kimi",
        &["Press ctrl-x: toggle mode to switch.", "✨ "],
        "prompt_toolbar",
    );
}

#[test]
fn the_session_picker_leaves_the_reading_alone() {
    assert_skip(
        "kimi",
        &[
            " SESSIONS (1 of 12)  [current directory] ",
            "> fix the build",
            "Enter to select · Esc to cancel ",
        ],
        "session_picker",
    );
    assert_not_rule("kimi", PROMPT, "session_picker");
}

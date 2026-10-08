//! Kilo Code CLI, screens built from the session view in `packages/tui/src` and the Kilo
//! code in `packages/opencode/src/kilocode`.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "┃  Ask anything... \"Fix a TODO in the codebase\"",
    "┃",
    "┃  Code  claude-sonnet-4",
    "~/project                                          ctrl+x a agents  ctrl+p commands",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn a_permission_panel_is_a_blocker() {
    assert_rule(
        "kilo",
        &[
            "  Checking the build.",
            "┃ △ Permission required",
            "┃   # Run outside the sandbox",
            "┃  $ cargo build",
            "┃  Allow   Reject                                  ⇆ select  enter confirm",
        ],
        "permission_panel",
        Blocked,
    );
    assert_rule(
        "kilo",
        &[
            "┃ △ Always allow",
            "┃   This will allow bash permanently.",
            "┃  Confirm   Cancel                               ⇆ select  enter confirm",
        ],
        "permission_panel",
        Blocked,
    );
}

#[test]
fn a_permission_panel_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "  Kilo shows:",
        "  △ Permission required",
        "  Allow once   Allow always   Reject   ⇆ select  enter confirm",
    ]);
    assert_not_rule("kilo", &screen, "permission_panel");
    assert_no_blocker("kilo", &screen);
}

#[test]
fn a_question_is_a_blocker() {
    let screen = [
        "┃  Which database?",
        "┃  1. Postgres",
        "┃  2. Type your own answer",
        "┃  ⇆ tab  ↑↓ select  enter submit  esc dismiss",
    ];
    assert_rule("kilo", &screen, "question_prompt", Blocked);
    let quoted = with_composer(&["  The footer reads: enter submit  esc dismiss"]);
    assert_not_rule("kilo", &quoted, "question_prompt");
    assert_no_blocker("kilo", &quoted);
}

#[test]
fn the_attention_title_is_a_blocker() {
    assert_rule_titled(
        "kilo",
        &[""],
        "⚠ Kilo CLI | fix the build",
        "title_attention",
        Blocked,
    );
    assert_not_rule_titled("kilo", &[""], "Kilo CLI | fix the build", "title_attention");
}

#[test]
fn the_busy_row_is_working() {
    assert_rule(
        "kilo",
        &[
            "┃  Ask anything... \"Fix a TODO in the codebase\"",
            "▰▰▱▱▱▱▱                    esc interrupt      8.1K (4%) · $0.02  ctrl+p commands",
        ],
        "busy_row",
        Working,
    );
    assert_rule(
        "kilo",
        &["┃  ", "[⋯] fetch failed [waiting for network]"],
        "busy_row",
        Working,
    );
    assert_rule(
        "kilo",
        &["┃  ", "[⋯] Overloaded [retrying in 4s attempt #2]"],
        "busy_row",
        Working,
    );
    assert_not_rule("kilo", COMPOSER, "busy_row");
}

#[test]
fn the_working_title_is_working() {
    assert_rule_titled("kilo", &[""], "◔ Kilo CLI | fix", "title_working", Working);
    assert_not_rule_titled("kilo", &[""], "✓ Kilo CLI | fix", "title_working");
}

#[test]
fn the_empty_composer_is_idle() {
    assert_rule("kilo", COMPOSER, "composer_placeholder", Idle);
    assert_not_rule(
        "kilo",
        &[
            "┃  Ask anything… \"Fix a TODO\"",
            "~/project      ctrl+p commands",
        ],
        "composer_placeholder",
    );
}

/// Fullscreen pushes the header out of the bottom rows; the buttons and the hint stay.
#[test]
fn fullscreen_permission_buttons_are_a_blocker() {
    let mut screen = vec!["  ┃  △ Permission required", "  ┃    → Edit src/lib.rs"];
    screen.extend(std::iter::repeat_n("  ┃   12   let total = 0;", 30));
    screen.extend([
        "  ┃",
        "  ┃   Allow once   Reject                  ctrl+f minimize  ⇆ select  enter confirm",
        "  ┃",
    ]);
    assert_rule("kilo", &screen, "permission_buttons", Blocked);
    let quoted = with_composer(&["  Allow once   Reject   ⇆ select"]);
    assert_not_rule("kilo", &quoted, "permission_buttons");
    assert_no_blocker("kilo", &quoted);
}

/// The block spinner on the busy row's left holds working when the hint is cut.
#[test]
fn the_busy_spinner_alone_is_working() {
    let screen = [
        "┃  Ask anything... \"Fix a TODO in the codebase\"",
        "   ⬝⬝■■■⬝⬝⬝ esc in  ctrl+p commands",
    ];
    assert_rule("kilo", &screen, "busy_row", Working);
}

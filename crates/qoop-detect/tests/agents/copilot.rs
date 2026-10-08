//! GitHub Copilot CLI, screens built from `cli-main.js` in its 1.0.92 package.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const BAND_TOP: &str = "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄";
const BAND_BOTTOM: &str = "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀";
const COMPOSER: &[&str] = &[BAND_TOP, "❯ ", BAND_BOTTOM, " / commands · ? help"];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn a_command_approval_is_a_blocker() {
    let screen = [
        "╭ Remove the build directory ─────────────╮",
        "│ ╭──────────────────────────────────────╮ │",
        "│ │ rm -rf build                         │ │",
        "│ ╰──────────────────────────────────────╯ │",
        "│ Do you want to run this command?         │",
        "│ ❯ 1. Yes                                 │",
        "│   2. Yes, and approve rm for the rest of the running session │",
        "│   3. No, and tell Copilot what to do differently │",
        "│ ↑/↓ to navigate · enter to select · esc to cancel │",
        "╰──────────────────────────────────────────╯",
    ];
    assert_rule("copilot", &screen, "permission_dialog", Blocked);
}

#[test]
fn an_approval_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "● It asks:",
        "  Do you want to run this command?",
        "  ↑/↓ to navigate · enter to select · esc to cancel",
    ]);
    assert_not_rule("copilot", &screen, "permission_dialog");
    assert_no_blocker("copilot", &screen);
}

#[test]
fn plan_review_is_a_blocker() {
    let screen = [
        "╭ Plan Ready for Review ──────────────────╮",
        "│ 1. Split the parser                      │",
        "│ ↑/↓ to navigate · enter to select · ctrl+e to show full plan · esc to cancel │",
        "╰──────────────────────────────────────────╯",
    ];
    assert_rule("copilot", &screen, "plan_review", Blocked);
    let quoted = with_composer(&["● Plan Ready for Review appears, ctrl+e to show full plan."]);
    assert_not_rule("copilot", &quoted, "plan_review");
    assert_no_blocker("copilot", &quoted);
}

#[test]
fn a_question_is_a_blocker() {
    let screen = [
        "╭ Question ───────────────────────────────╮",
        "│ Which database?                          │",
        "│ ❯ 1. Postgres                            │",
        "│   2. Other (type your answer below)      │",
        "╰──────────────────────────────────────────╯",
    ];
    assert_rule("copilot", &screen, "question_dialog", Blocked);
    let quoted = with_composer(&["Question", "  2. Other (type your answer below)"]);
    assert_not_rule("copilot", &quoted, "question_dialog");
    assert_no_blocker("copilot", &quoted);
}

#[test]
fn folder_trust_is_a_setup_gate() {
    let screen = [
        "╭ Confirm folder trust ───────────────────╮",
        "│ /home/me/project                         │",
        "│ Do you trust the files in this folder?   │",
        "│ ❯ 1. Yes                                 │",
        "│   2. Yes, and remember this folder for future sessions │",
        "│   3. No                                  │",
        "╰──────────────────────────────────────────╯",
    ];
    assert_rule("copilot", &screen, "folder_trust", Blocked);
    assert!(verdict("copilot", &screen).is_some_and(|v| v.setup_gate));
    let quoted = with_composer(&[
        "● It asks:",
        "  Confirm folder trust",
        "  Do you trust the files in this folder?",
        "  ❯ 1. Yes",
    ]);
    assert_not_rule("copilot", &quoted, "folder_trust");
    assert_no_blocker("copilot", &quoted);
}

#[test]
fn the_working_line_is_working() {
    assert_rule(
        "copilot",
        &with_composer(&["◉ Reading the parser · 2.1 KB (Esc to cancel)"]),
        "esc_to_cancel",
        Working,
    );
    assert_not_rule(
        "copilot",
        &with_composer(&["● Cancelling"]),
        "esc_to_cancel",
    );
}

#[test]
fn the_composer_band_is_idle() {
    assert_rule("copilot", COMPOSER, "composer", Idle);
    assert_not_rule("copilot", &["❯ fix the build", "● done"], "composer");
}

#[test]
fn the_model_picker_leaves_the_reading_alone() {
    assert_skip(
        "copilot",
        &["╭ Select Model for this session ─╮", "│ ❯ 1. gpt-5 │"],
        "model_picker",
    );
    assert_not_rule("copilot", COMPOSER, "model_picker");
}

/// A live trust dialog under a previous session's composer band.
#[test]
fn a_live_gate_under_scrollback_is_still_a_gate() {
    let mut screen = COMPOSER.to_vec();
    screen.extend([
        "$ cd ../other && copilot",
        "╭ Confirm folder trust ───────────────────╮",
        "│ Do you trust the files in this folder?   │",
        "│ ❯ 1. Yes                                 │",
        "╰──────────────────────────────────────────╯",
    ]);
    assert_rule("copilot", &screen, "folder_trust", Blocked);
}

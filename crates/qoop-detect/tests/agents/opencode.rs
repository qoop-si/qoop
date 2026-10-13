//! OpenCode, screens built from the session view in `packages/tui/src`.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "┃  Ask anything… \"Fix a TODO in the codebase\"",
    "┃",
    "┃  Build  claude-sonnet-4",
    "~/project                                          ctrl+x a agents  ctrl+p commands",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

const PERMISSION: &[&str] = &[
    "┃ △ Permission required",
    "┃   # Shell command",
    "┃",
    "┃  $ rm -rf build",
    "┃",
    "┃  Allow once   Allow always   Reject              ⇆ select  enter confirm",
];

#[test]
fn a_permission_panel_is_a_blocker() {
    let mut screen = vec!["  Removing the old build first."];
    screen.extend(PERMISSION);
    assert_rule("opencode", &screen, "permission_panel", Blocked);
    assert_rule(
        "opencode",
        &[
            "┃ △ Always allow",
            "┃   This will allow bash until OpenCode is restarted.",
            "┃  Confirm   Cancel                               ⇆ select  enter confirm",
        ],
        "permission_panel",
        Blocked,
    );
    assert_rule(
        "opencode",
        &[
            "┃ △ Reject permission",
            "┃   Tell OpenCode what to do differently",
            "┃",
            "┃                                                 enter confirm  esc cancel",
        ],
        "permission_panel",
        Blocked,
    );
}

#[test]
fn a_permission_panel_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "  OpenCode shows:",
        "  △ Permission required",
        "  Allow once   Allow always   Reject   ⇆ select  enter confirm",
    ]);
    assert_not_rule("opencode", &screen, "permission_panel");
    assert_no_blocker("opencode", &screen);
}

#[test]
fn a_question_is_a_blocker() {
    let screen = [
        "┃  Which database?",
        "┃  1. Postgres",
        "┃  2. Type your own answer",
        "┃  ⇆ tab  ↑↓ select  enter submit  esc dismiss",
    ];
    assert_rule("opencode", &screen, "question_prompt", Blocked);
    let quoted = with_composer(&["  The footer reads: ↑↓ select  enter submit  esc dismiss"]);
    assert_not_rule("opencode", &quoted, "question_prompt");
    assert_no_blocker("opencode", &quoted);
}

#[test]
fn the_busy_row_is_working() {
    let busy = [
        "┃  Ask anything… \"Fix a TODO in the codebase\"",
        "┃",
        "┃  Build  claude-sonnet-4",
        "▰▰▱▱▱▱▱                    esc interrupt      8.1K (4%) · $0.02  ctrl+p commands",
    ];
    assert_rule("opencode", &busy, "busy_row", Working);
    assert_rule(
        "opencode",
        &[
            "┃  Ask anything… \"Fix a TODO in the codebase\"",
            "[⋯]                                                esc again to interrupt",
        ],
        "busy_row",
        Working,
    );
    assert_rule(
        "opencode",
        &["┃  ", "[⋯] Overloaded [retrying in 4s attempt #2]"],
        "busy_row",
        Working,
    );
    assert_not_rule("opencode", COMPOSER, "busy_row");
    assert_not_rule(
        "opencode",
        &with_composer(&["  Press esc interrupt to stop a turn."]),
        "busy_row",
    );
}

#[test]
fn the_empty_composer_is_idle() {
    assert_rule("opencode", COMPOSER, "composer_placeholder", Idle);
    assert_rule(
        "opencode",
        &[
            "┃  Run a command… \"git status\"",
            "~/project      esc exit shell mode",
        ],
        "composer_placeholder",
        Idle,
    );
    assert_not_rule(
        "opencode",
        &["┃  fix the build", "~/project      ctrl+p commands"],
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
        "  ┃   Allow once   Allow always   Reject       ctrl+f minimize  ⇆ select  enter confirm",
        "  ┃",
    ]);
    assert_rule("opencode", &screen, "permission_buttons", Blocked);
    let quoted = with_composer(&["  Allow once   Allow always   Reject   ⇆ select"]);
    assert_not_rule("opencode", &quoted, "permission_buttons");
    assert_no_blocker("opencode", &quoted);
}

/// A narrow screen cuts the hint and keeps the buttons under the header.
#[test]
fn a_cut_permission_row_is_a_blocker() {
    let screen = [
        "  ┃  △ Permission required",
        "  ┃",
        "  ┃   Allow once   Allow always   Reject       ctrl+f fullscreen  ⇆ select  enter co",
        "  ┃",
    ];
    assert_rule("opencode", &screen, "permission_panel", Blocked);
}

/// The block spinner on the busy row's left holds working when the hint is cut.
#[test]
fn the_busy_spinner_alone_is_working() {
    let screen = [
        "┃  Ask anything… \"Fix a TODO in the codebase\"",
        "┃  Build  claude-sonnet-4",
        "   ⬝⬝■■■⬝⬝⬝ esc in  ctrl+p commands",
    ];
    assert_rule("opencode", &screen, "busy_row", Working);
}

/// A transcript line ending in "<word> commands" above a live panel is not the composer's
/// status row: only the screen's last line is read for that.
#[test]
fn a_transcript_line_ending_in_commands_does_not_hide_a_panel() {
    let screen = [
        "     I will run the cleanup commands",
        "",
        "  ┃",
        "  ┃  △ Reject permission",
        "  ┃",
        "  ┃  Tell OpenCode what to do differently",
        "  ┃",
        "  ┃  use make clean instead     enter confirm  esc cancel",
        "  ┃",
        "",
    ];
    assert_rule("opencode", &screen, "permission_panel", Blocked);
}

/// The composer's placeholder quoted in an answer above a live Reject panel: the panel is
/// at the bottom and the quote is not the composer.
#[test]
fn a_quoted_placeholder_above_a_live_panel_does_not_hide_it() {
    let screen = [
        "     The empty prompt reads Ask anything… \"Fix a TODO\"",
        "",
        "  ┃",
        "  ┃  △ Reject permission",
        "  ┃",
        "  ┃  Tell OpenCode what to do differently",
        "  ┃",
        "  ┃  use make clean instead     enter confirm  esc cancel",
        "  ┃",
        "",
    ];
    let v = verdict("opencode", &screen).expect("a verdict");
    assert_eq!(v.state, Blocked, "{v:?}");
    assert!(v.visible_blocker);
    // The Reject stage's input row alone at the bottom is still the panel.
    assert_rule(
        "opencode",
        &[
            "  ┃",
            "  ┃  use make clean instead     enter confirm  esc cancel",
            "  ┃",
        ],
        "permission_buttons",
        Blocked,
    );
}

/// opencode 2 shares one background service whose environment is the first tab's; a tab that
/// comes back must run its own server, or its hook reports would land on that first terminal.
#[test]
fn a_restored_tab_runs_its_own_server() {
    assert_eq!(
        qoop_detect::resume_argv("opencode", Some("ses_x")),
        Some(vec![
            "opencode".to_string(),
            "--standalone".to_string(),
            "--session".to_string(),
            "ses_x".to_string()
        ])
    );
    assert_eq!(
        qoop_detect::resume_argv("opencode", None),
        Some(vec![
            "opencode".to_string(),
            "--standalone".to_string(),
            "--continue".to_string()
        ])
    );
}

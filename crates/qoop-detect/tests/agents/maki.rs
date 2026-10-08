//! maki, screens built from `maki-ui/src`.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "╭──────────────────────────────────────────────╮",
    "│ ❯ Ask maki to fix a bug...                   │",
    "╰──────────────────────────────────────────────╯",
    " build                                12% $0.03",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn the_permission_prompt_is_a_blocker() {
    let mut screen = vec![
        "  an earlier answer",
        "      ┌ Permission Required ───────────────────┐",
        "      │  tool  bash                            │",
        "      │  scope rm -rf build                    │",
        "      │  y  Allow   a  Always (project)        │",
        "      │  n  Deny    d  Deny-always (project)   │",
        "      └────────────────────────────────────────┘",
    ];
    screen.extend(COMPOSER);
    assert_rule("maki", &screen, "permission_prompt", Blocked);
    assert_rule(
        "maki",
        &[
            "╭ Permission Required ─────────────────────╮",
            "│  Enter / y  Confirm allow (session)      │",
            "│  any  Cancel                             │",
            "╰──────────────────────────────────────────╯",
        ],
        "permission_prompt",
        Blocked,
    );
}

#[test]
fn the_permission_prompt_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "  The prompt is titled Permission Required and offers:",
        "  y  Allow",
        "  n  Deny",
    ]);
    assert_not_rule("maki", &screen, "permission_prompt");
    assert_no_blocker("maki", &screen);
}

#[test]
fn the_trust_card_is_a_setup_gate() {
    let screen = [
        "┌ Folder Trust ────────────────────────────────────────────────┐",
        "│   Trust this folder \u{2014} load its shared config now             │",
        "│ > Not now \u{2014} stay restricted this run, ask again next start   │",
        "│   Never \u{2014} remember the no and stop asking                    │",
        "│ t/y  Trust   n  Not now   ↑/↓  Select   Enter  Confirm       │",
        "└──────────────────────────────────────────────────────────────┘",
    ];
    assert_rule("maki", &screen, "folder_trust", Blocked);
    assert!(verdict("maki", &screen).is_some_and(|v| v.setup_gate));
    let quoted = with_composer(&[
        "  The card reads:",
        "  ┌ Folder Trust ──────────────┐",
        "  │ > Trust this folder        │",
        "  └────────────────────────────┘",
    ]);
    assert_not_rule("maki", &quoted, "folder_trust");
    assert_no_blocker("maki", &quoted);
}

#[test]
fn the_status_spinner_is_working() {
    let screen = [
        "╭──────────────────────────────────────────────╮",
        "│ ❯ Queue another prompt...                    │",
        "╰──────────────────────────────────────────────╯",
        " ⠹ build                              12% $0.03",
    ];
    assert_rule("maki", &screen, "status_spinner", Working);
    assert_not_rule("maki", COMPOSER, "status_spinner");
}

#[test]
fn the_working_composer_is_working() {
    assert_rule(
        "maki",
        &["│ ❯ Queue a message for this subagent...       │"],
        "working_composer",
        Working,
    );
    assert_rule(
        "maki",
        &["│ ❯ ", " Press esc again to stop..."],
        "working_composer",
        Working,
    );
    assert_not_rule("maki", COMPOSER, "working_composer");
}

#[test]
fn the_empty_composer_is_idle() {
    assert_rule("maki", COMPOSER, "composer_placeholder", Idle);
    assert_not_rule(
        "maki",
        &["│ ❯ fix the parser                             │"],
        "composer_placeholder",
    );
}

/// A live trust card under a previous session's composer.
#[test]
fn a_live_gate_under_scrollback_is_still_a_gate() {
    let mut screen = COMPOSER.to_vec();
    screen.extend([
        "$ cd ../other && maki",
        "┌ Folder Trust ────────────────────────────────────────────────┐",
        "│   Trust this folder \u{2014} load its shared config now             │",
        "│ > Not now \u{2014} stay restricted this run, ask again next start   │",
        "└──────────────────────────────────────────────────────────────┘",
    ]);
    assert_rule("maki", &screen, "folder_trust", Blocked);
}

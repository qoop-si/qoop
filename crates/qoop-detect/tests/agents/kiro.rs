//! Kiro CLI, screens built from the terminal interface bundled in its 2.28.0 package.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "> ask a question or describe a task ↵",
    "  claude-sonnet-4 · 12%",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn an_approval_dialog_is_a_blocker() {
    let screen = [
        "  shell requires approval",
        "  execute → rm -rf build",
        "  ❯ Yes, single permission",
        "    Trust, always allow in this session",
        "    No",
        "  y approve · n deny · t trust",
        "> Kiro is working · Type to queue",
    ];
    assert_rule("kiro", &screen, "approval_dialog", Blocked);
    assert_rule(
        "kiro",
        &[
            "  fs_write requires approval",
            "  > Yes, single permission",
            "    No",
            "  y approve . n deny . t trust",
        ],
        "approval_dialog",
        Blocked,
    );
}

#[test]
fn an_approval_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "Kiro asks first:",
        "  shell requires approval",
        "  y approve · n deny · t trust",
    ]);
    assert_not_rule("kiro", &screen, "approval_dialog");
    assert_no_blocker("kiro", &screen);
}

#[test]
fn the_monitor_banner_is_a_blocker() {
    assert_rule(
        "kiro",
        &[
            "Crew",
            "⚠ The agent needs permission to run a tool │ press q or Esc to return to chat and respond",
            "  worker-1  running",
        ],
        "monitor_banner",
        Blocked,
    );
    assert_not_rule(
        "kiro",
        &with_composer(&["The agent is asking a question in another view."]),
        "monitor_banner",
    );
}

#[test]
fn startup_gates_are_setup_gates() {
    let warning = [
        "────────────────────────────────────────",
        "Warning: Kiro is running in trust all tools mode",
        "This mode is intended for sandboxed or disposable environments only.",
        "❯ No, exit",
        "  Yes, I accept",
        "  Yes, and don't ask again",
        "────────────────────────────────────────",
    ];
    assert_rule("kiro", &warning, "trust_all_tools_warning", Blocked);
    assert!(verdict("kiro", &warning).is_some_and(|v| v.setup_gate));
    let quoted = with_composer(&[
        "It said:",
        "Warning: Kiro is running in trust all tools mode",
        "❯ No, exit",
    ]);
    assert_not_rule("kiro", &quoted, "trust_all_tools_warning");
    assert_no_blocker("kiro", &quoted);

    let sign_in = [
        "Welcome to Kiro, let's get you signed in!",
        "Press enter to continue to the browser or esc to cancel",
    ];
    assert_rule("kiro", &sign_in, "sign_in", Blocked);
    let quoted = with_composer(&[
        "You will see:",
        "Press enter to continue to the browser or esc to cancel",
    ]);
    assert_not_rule("kiro", &quoted, "sign_in");
    assert_no_blocker("kiro", &quoted);
}

#[test]
fn the_busy_row_is_working() {
    for line in [
        "⠹ Thinking... (esc to cancel)",
        "⠹ Still thinking, this is a tricky one... (ctrl+c to cancel)",
    ] {
        assert_rule(
            "kiro",
            &[
                line,
                "> Kiro is working · 12s · Type to steer · tab to queue",
            ],
            "busy_row",
            Working,
        );
    }
    assert_not_rule("kiro", COMPOSER, "busy_row");
}

#[test]
fn the_working_placeholder_is_working() {
    assert_rule(
        "kiro",
        &["> Kiro is working · Type to queue"],
        "working_placeholder",
        Working,
    );
    assert_rule(
        "kiro",
        &["> Initializing · type to queue a message"],
        "working_placeholder",
        Working,
    );
    assert_not_rule("kiro", COMPOSER, "working_placeholder");
}

#[test]
fn the_composer_is_idle() {
    assert_rule("kiro", COMPOSER, "composer_placeholder", Idle);
    assert_rule(
        "kiro",
        &["> ask a question or describe a task ↵  ·  exit plan mode: shift+tab"],
        "composer_placeholder",
        Idle,
    );
    assert_not_rule(
        "kiro",
        &["> fix the build", "  claude-sonnet-4 · 12%"],
        "composer_placeholder",
    );
}

#[test]
fn the_title_reads_working_and_ready() {
    assert_rule_titled("kiro", &[""], "◑ kiro: ~/project", "title_working", Working);
    assert_rule_titled("kiro", &[""], "| kiro: ~/project", "title_working", Working);
    assert_not_rule_titled("kiro", &[""], "kiro: ~/project", "title_working");
    assert_rule_titled("kiro", &[""], "kiro: ~/project", "title_ready", Idle);
    assert_not_rule_titled("kiro", &[""], "zsh", "title_ready");
}

/// A live trust-all warning under a previous session's composer.
#[test]
fn a_live_gate_under_scrollback_is_still_a_gate() {
    let mut screen = COMPOSER.to_vec();
    screen.extend([
        "$ kiro-cli --trust-all-tools",
        "Warning: Kiro is running in trust all tools mode",
        "❯ No, exit",
        "  Yes, I accept",
    ]);
    assert_rule("kiro", &screen, "trust_all_tools_warning", Blocked);
}

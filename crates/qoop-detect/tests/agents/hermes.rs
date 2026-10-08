//! Hermes Agent, screens built from its classic prompt_toolkit interface and its Ink one.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const RULE: &str = "────────────────────────────────────────────────────────────";

/// The classic composer at rest.
const COMPOSER: &[&str] = &[RULE, "❯ Find and fix a failing test", RULE];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn the_classic_approval_panel_is_a_blocker() {
    let screen = [
        "╭─ ⚠️  Dangerous Command ──────────────────────────╮",
        "│                                                   │",
        "│ rm -rf build                                      │",
        "│                                                   │",
        "│ ❯ 1. Allow once                                   │",
        "│   2. Allow for this session                       │",
        "│   3. Add to permanent allowlist                   │",
        "│   4. Deny                                         │",
        "╰───────────────────────────────────────────────────╯",
        "↑/↓ to select, Enter to confirm",
    ];
    assert_rule("hermes", &screen, "dangerous_command_panel", Blocked);
    let quoted = with_composer(&[
        "It shows a Dangerous Command panel:",
        "❯ 1. Allow once",
        "  4. Deny",
    ]);
    assert_not_rule("hermes", &quoted, "dangerous_command_panel");
    assert_no_blocker("hermes", &quoted);
}

#[test]
fn the_plain_prompt_is_a_blocker() {
    let screen = [
        "⚠️  DANGEROUS COMMAND: delete the build directory",
        "      rm -rf build",
        "      [o]nce  |  [s]ession  |  [a]lways  |  [d]eny",
        "      Choice [o/s/a/D]: ",
    ];
    assert_rule("hermes", &screen, "dangerous_command_prompt", Blocked);
    let quoted = with_composer(&["      Choice [o/s/a/D]: "]);
    assert_not_rule("hermes", &quoted, "dangerous_command_prompt");
    assert_no_blocker("hermes", &quoted);
}

#[test]
fn ink_overlays_are_blockers() {
    for hint in [
        "║ ↑/↓ select · Enter confirm · 1-4 quick pick · Esc/Ctrl+C deny ║",
        "║ ↑/↓ select · Enter confirm · Y/N quick · Esc cancel ║",
        "║ ↑/↓ select · Enter submit · Tab/Shift+Tab switch question · Esc/Ctrl+C cancel ║",
    ] {
        let screen = [
            "╔══════════════════════════════════════════════╗",
            "║ ⚠ approval required · delete the build       ║",
            "║ ▸ 1. Allow once                              ║",
            hint,
            "╚══════════════════════════════════════════════╝",
        ];
        assert_rule("hermes", &screen, "ink_prompt", Blocked);
    }
    let quoted = with_composer(&["↑/↓ select · Enter confirm · 1-4 quick pick · Esc/Ctrl+C deny"]);
    assert_not_rule("hermes", &quoted, "ink_prompt");
    assert_no_blocker("hermes", &quoted);
}

#[test]
fn a_classic_modal_prompt_is_a_blocker() {
    assert_rule(
        "hermes",
        &[
            "╭─ Confirm action ─╮",
            "│ 1. Yes │",
            "╰──────────────────╯",
            RULE,
            "❯ type 1/2/3, or ↑/↓ to select, Enter to confirm",
            RULE,
        ],
        "classic_modal_prompt",
        Blocked,
    );
    let quoted = with_composer(&["It said: type your answer here and press Enter"]);
    assert_not_rule("hermes", &quoted, "classic_modal_prompt");
    assert_no_blocker("hermes", &quoted);
}

#[test]
fn the_ink_attention_title_is_a_blocker() {
    assert_rule_titled(
        "hermes",
        &[""],
        "⚠ fix the build · hermes-4 · ~/project",
        "title_attention",
        Blocked,
    );
    assert_not_rule_titled("hermes", &[""], "Hermes", "title_attention");
}

#[test]
fn the_classic_running_placeholder_is_working() {
    assert_rule(
        "hermes",
        &[
            RULE,
            "❯ msg=interrupt · /queue · /bg · /steer · Ctrl+C cancel",
            RULE,
        ],
        "classic_running_placeholder",
        Working,
    );
    assert_not_rule("hermes", COMPOSER, "classic_running_placeholder");
}

#[test]
fn the_classic_spinner_is_working() {
    assert_rule(
        "hermes",
        &with_composer(&["  ⠹ (◔_◔) pondering... (3.2s)"]),
        "classic_spinner",
        Working,
    );
    assert_not_rule(
        "hermes",
        &with_composer(&["I kept pondering the parser for a while."]),
        "classic_spinner",
    );
}

#[test]
fn the_ink_interrupt_hint_and_title_are_working() {
    assert_rule(
        "hermes",
        &["❯ ", "  Ctrl+C to interrupt…"],
        "ink_interrupt_hint",
        Working,
    );
    assert_not_rule("hermes", COMPOSER, "ink_interrupt_hint");
    assert_rule_titled("hermes", &[""], "⏳ fix the build", "title_busy", Working);
    assert_not_rule_titled("hermes", &[""], "✓ fix the build", "title_busy");
}

#[test]
fn the_classic_composer_and_the_ink_title_are_idle() {
    assert_rule("hermes", COMPOSER, "classic_composer", Idle);
    assert_not_rule("hermes", &[RULE, "  a table row", RULE], "classic_composer");
    assert_rule_titled("hermes", &[""], "✓ fix the build", "title_ready", Idle);
    assert_not_rule_titled("hermes", &[""], "Hermes", "title_ready");
}

#[test]
fn the_ink_palette_leaves_the_reading_alone() {
    assert_skip(
        "hermes",
        &[
            "/help",
            "Type to filter 42 commands - ↑/↓ then Enter inserts, Esc cancels",
        ],
        "palette_open",
    );
    assert_not_rule("hermes", COMPOSER, "palette_open");
}

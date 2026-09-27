//! Devin CLI, screens built from the strings in its 3000.11.3 release and the transcripts
//! on devin.ai/cli.

use super::*;
use AgentRuntimeState::{Idle, Working};

const COMPOSER: &[&str] = &[
    "❭Ask Devin to build features, fix bugs, or work on your code",
    "  Claude Sonnet 5 Medium",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

#[test]
fn the_interrupt_hint_is_working() {
    assert_rule(
        "devin",
        &[
            "⡆ Thinking · 0s (esc twice to interrupt)",
            "❭Guide Devin while it works",
        ],
        "interrupt_hint",
        Working,
    );
    assert_rule(
        "devin",
        &["⡆ Thinking · 4s (esc again to interrupt)"],
        "interrupt_hint",
        Working,
    );
    assert_not_rule(
        "devin",
        &with_composer(&["Press esc twice to interrupt a running turn."]),
        "interrupt_hint",
    );
}

#[test]
fn the_working_placeholder_is_working() {
    assert_rule(
        "devin",
        &["❭Guide Devin while it works", "  Claude Sonnet 5 Medium"],
        "working_placeholder",
        Working,
    );
    assert_not_rule("devin", COMPOSER, "working_placeholder");
}

#[test]
fn the_empty_composer_is_idle() {
    assert_rule("devin", COMPOSER, "composer_placeholder", Idle);
    assert_not_rule(
        "devin",
        &["❭fix the build", "  Claude Sonnet 5 Medium"],
        "composer_placeholder",
    );
}

#[test]
fn the_model_picker_leaves_the_reading_alone() {
    assert_skip(
        "devin",
        &[
            "/ Type to search",
            "● Claude Sonnet 5",
            "↑↓ select · tab lead · ←→ effort · ↵ confirm · esc cancel",
        ],
        "model_picker",
    );
    assert_not_rule("devin", COMPOSER, "model_picker");
}

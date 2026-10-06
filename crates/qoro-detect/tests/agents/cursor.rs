//! Cursor CLI, screens built from its bundled interface JavaScript.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const PROMPT: &[&str] = &[
    "→ Add a follow-up",
    "  claude-4.5-sonnet · 12% · 1 file edited",
    "  ~/project · main",
];

fn with_prompt(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(PROMPT).copied().collect()
}

#[test]
fn a_command_approval_is_a_blocker() {
    let screen = [
        "  $ rm -rf build",
        "  Run this command?",
        " → Run (once) (y)",
        "   Add Shell(rm) to allowlist? (tab)",
        "   Skip & tell the agent what to do instead (esc or n)",
        "→ Waiting for decision (y/n/p)...",
        "  claude-4.5-sonnet · 12%",
    ];
    assert_rule("cursor", &screen, "decision_surface", Blocked);
}

#[test]
fn an_approval_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_prompt(&["It asks:", "Run this command?", " → Run (once) (y)"]);
    assert_not_rule("cursor", &screen, "decision_surface");
    assert_no_blocker("cursor", &screen);
}

#[test]
fn plan_approval_is_a_blocker() {
    let screen = [
        "  1. Split the parser",
        "  ────────────────",
        "  Ready to build?",
        " → 1. Yes, build locally (b)",
        "   2. Yes, build in cloud (c)",
        "   3. No, propose changes (p or Esc)",
    ];
    assert_rule("cursor", &screen, "plan_approval", Blocked);
    let quoted = with_prompt(&["Ready to build?", "1. Yes, build locally"]);
    assert_not_rule("cursor", &quoted, "plan_approval");
    assert_no_blocker("cursor", &quoted);
}

#[test]
fn a_waiting_prompt_is_a_blocker() {
    for line in [
        "→ Approve mode switch (y/n)",
        "→ Answer questions (Enter to select/next, Esc to skip)",
        "→ Tell the agent what to do instead (Enter to send, empty to skip, Esc to cancel)",
    ] {
        assert_rule(
            "cursor",
            &[line, "  claude-4.5-sonnet · 12%"],
            "waiting_prompt",
            Blocked,
        );
    }
    let quoted = with_prompt(&["The prompt then says Approve mode switch (y/n)"]);
    assert_not_rule("cursor", &quoted, "waiting_prompt");
    let quoted = with_prompt(&["Approve mode switch (y/n)"]);
    assert_no_blocker("cursor", &quoted);
}

#[test]
fn the_title_reads_each_state() {
    assert_rule_titled(
        "cursor",
        &[""],
        "Fix parser - 🔐 Waiting for confirmation",
        "title_waiting",
        Blocked,
    );
    assert_rule_titled(
        "cursor",
        &[""],
        "Cursor Agent - ❓ Waiting for you",
        "title_waiting",
        Blocked,
    );
    assert_rule_titled(
        "cursor",
        &[""],
        "Fix parser - ⏳ Working ..·",
        "title_working",
        Working,
    );
    assert_rule_titled(
        "cursor",
        &[""],
        "Fix parser - ✅ Ready",
        "title_ready",
        Idle,
    );
    assert_not_rule_titled("cursor", &[""], "Cursor Agent", "title_ready");
    assert_not_rule_titled("cursor", &[""], "Fix parser - ✅ Ready", "title_working");
    assert_not_rule_titled("cursor", &[""], "Fix parser - ⏳ Working", "title_waiting");
    // The status is the last part; the same words in the chat name do not count.
    assert_not_rule_titled(
        "cursor",
        &[""],
        "Docs - Waiting for confirmation - ✅ Ready",
        "title_waiting",
    );
    assert_not_rule_titled(
        "cursor",
        &[""],
        "Fix - Working directory handling - ✅ Ready",
        "title_working",
    );

    assert_rule_titled(
        "cursor",
        &[""],
        "Fix - Working tree cleanup - ✅ Ready",
        "title_ready",
        Idle,
    );
    assert_not_rule_titled(
        "cursor",
        &[""],
        "Fix - Working tree cleanup - ✅ Ready",
        "title_working",
    );
    assert_rule_titled(
        "cursor",
        &[""],
        "Fix parser - 🔐 Waiting for confirmation (fix-parser)",
        "title_waiting",
        Blocked,
    );
}

#[test]
fn workspace_trust_is_a_setup_gate() {
    let screen = [
        "Workspace Trust Required",
        "Cursor Agent can execute code and access files in this directory.",
        "Do you trust the contents of this directory?",
        "→ Trust this workspace (a)",
        "  Quit (q)",
    ];
    assert_rule("cursor", &screen, "workspace_trust", Blocked);
    assert!(verdict("cursor", &screen).is_some_and(|v| v.setup_gate));
    let quoted = with_prompt(&[
        "It asked:",
        "Workspace Trust Required",
        "Do you trust the contents of this directory?",
        "→ Trust this workspace (a)",
    ]);
    assert_not_rule("cursor", &quoted, "workspace_trust");
    assert_no_blocker("cursor", &quoted);
}

#[test]
fn the_generating_row_is_working() {
    assert_rule(
        "cursor",
        &with_prompt(&["⠰⠰ Thinking 1.2k tokens"]),
        "generating_row",
        Working,
    );
    assert_not_rule("cursor", PROMPT, "generating_row");
}

#[test]
fn the_stop_hint_is_working() {
    assert_rule(
        "cursor",
        &[
            "→                                          ctrl+c to stop",
            "  ~/project",
        ],
        "ctrl_c_to_stop",
        Working,
    );
    assert_not_rule(
        "cursor",
        &with_prompt(&["Press ctrl+c to stop it."]),
        "ctrl_c_to_stop",
    );
}

#[test]
fn the_empty_prompt_is_idle() {
    assert_rule("cursor", PROMPT, "prompt_placeholder", Idle);
    assert_rule(
        "cursor",
        &["→ Plan, search, build anything"],
        "prompt_placeholder",
        Idle,
    );
    assert_not_rule("cursor", &["→ fix the build"], "prompt_placeholder");
}

#[test]
fn pagers_leave_the_reading_alone() {
    assert_skip(
        "cursor",
        &["Loading sessions...", "Press Ctrl-D, q or ESC to cancel"],
        "pager_open",
    );
    assert_skip(
        "cursor",
        &[
            "  MCP servers",
            "  Enter to select • d to delete • Esc to close",
        ],
        "pager_open",
    );
    assert_not_rule("cursor", PROMPT, "pager_open");
}

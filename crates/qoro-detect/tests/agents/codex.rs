//! Codex CLI, screens built from `codex-rs/tui` and its snapshot fixtures.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const COMPOSER: &[&str] = &[
    "› Ask Codex to do anything",
    "",
    "  ? for shortcuts                                                100% context left",
];

fn with_composer(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

const EXEC_APPROVAL: &[&str] = &[
    "• I need to read the file first.",
    "",
    "  Would you like to run the following command?",
    "",
    "  Reason: need filesystem access",
    "",
    "  $ cat /tmp/readme.txt",
    "",
    "› 1. Yes, proceed (y)",
    "  2. Yes, and don't ask again for commands that start with `echo hello world` (p)",
    "  3. No, and tell Codex what to do differently (esc)",
    "",
    "  Press enter to confirm or esc to cancel",
];

#[test]
fn an_approval_overlay_is_a_blocker() {
    assert_rule("codex", EXEC_APPROVAL, "approval_overlay", Blocked);
    assert_rule(
        "codex",
        &[
            "  Do you want to approve network access to \"example.com\"?",
            "",
            "  Reason: network request blocked",
            "",
            "› 1. Yes, just this once (y)",
            "  2. Yes, and allow this host for this conversation (a)",
            "  3. Yes, and allow this host in the future (p)",
            "  4. No, and tell Codex what to do differently (esc)",
            "",
            "  Press enter to confirm or esc to cancel or o to open thread",
        ],
        "approval_overlay",
        Blocked,
    );
}

#[test]
fn an_approval_quoted_in_an_answer_is_not_a_blocker() {
    let screen = with_composer(&[
        "• The dialog reads:",
        "  Would you like to run the following command?",
        "› 1. Yes, proceed (y)",
        "  Press enter to confirm or esc to cancel",
        "",
    ]);
    assert_not_rule("codex", &screen, "approval_overlay");
    assert_no_blocker("codex", &screen);
}

#[test]
fn the_action_required_title_is_a_blocker() {
    assert_rule_titled(
        "codex",
        &["› Ask Codex to do anything"],
        "[ ! ] Action Required | qoro",
        "title_action_required",
        Blocked,
    );
    assert_rule_titled(
        "codex",
        &[""],
        "[ . ] Action Required | qoro",
        "title_action_required",
        Blocked,
    );
    assert_not_rule_titled(
        "codex",
        &["  Would you like to run the following command?"],
        "qoro",
        "title_action_required",
    );
}

#[test]
fn the_trust_screen_is_a_setup_gate() {
    let screen = [
        "  Folder access",
        "",
        "  /tmp/project",
        "",
        "  Trust this folder? Codex can read, edit, and run files here, subject to your",
        "  permission settings.",
        "",
        "› 1. Trust and continue",
        "  2. Quit",
        "",
        "  enter continue and create sandbox · esc quit",
    ];
    assert_rule("codex", &screen, "trust_folder", Blocked);
    assert!(verdict("codex", &screen).is_some_and(|v| v.setup_gate));
    // The same lines quoted in an answer, with the composer under them.
    let quoted = with_composer(&[
        "• The first run shows:",
        "  Folder access",
        "› 1. Trust and continue",
        "  2. Quit",
        "  enter continue · esc quit",
        "• That is all.",
        "",
    ]);
    assert_not_rule("codex", &quoted, "trust_folder");
    assert_no_blocker("codex", &quoted);
}

#[test]
fn the_status_line_is_working() {
    for line in [
        "• Working (0s • esc to interrupt)",
        "• Investigating rendering code (12s • esc to interrupt)",
        "Thinking (1m 05s • esc to interrupt) · 1 background terminal running",
        "• Working (11s · esc to interrupt)",
    ] {
        assert_rule(
            "codex",
            &[
                line,
                "",
                "› Ask Codex to do anything",
                "",
                "  tab to queue message                                 100% context left",
            ],
            "status_interrupt",
            Working,
        );
    }
    // The same line inside a finished answer, with the idle composer under it.
    let quoted = with_composer(&[
        "• The status line reads like this:",
        "  Working (2s • esc to interrupt)",
        "",
    ]);
    assert_not_rule("codex", &quoted, "status_interrupt");
    let quoted_at_the_margin = with_composer(&["• Working (2s • esc to interrupt)", ""]);
    assert_not_rule("codex", &quoted_at_the_margin, "status_interrupt");

    assert_not_rule(
        "codex",
        &with_composer(&[
            "■ Conversation interrupted - use /feedback if something went wrong",
            "",
        ]),
        "status_interrupt",
    );
}

#[test]
fn the_queue_footer_is_working() {
    let screen = [
        "› ",
        "",
        "  tab to queue message                                           100% context left",
    ];
    assert_rule("codex", &screen, "footer_queue_message", Working);
    assert_not_rule("codex", COMPOSER, "footer_queue_message");
}

#[test]
fn the_spinner_title_is_working() {
    assert_rule_titled(
        "codex",
        &[""],
        "⠹ fix the tests | qoro",
        "title_spinner",
        Working,
    );
    assert_not_rule_titled("codex", &[""], "fix the tests | qoro", "title_spinner");
}

#[test]
fn the_composer_is_idle() {
    assert_rule(
        "codex",
        &[
            "› Ask Codex to do anything",
            "",
            "  gpt-5 default · /tmp/project",
        ],
        "composer",
        Idle,
    );
    assert_rule("codex", &["› Ask a follow-up question"], "composer", Idle);
    assert_rule(
        "codex",
        &["› write me a test", "  gpt-5 default · /tmp/project"],
        "composer",
        Idle,
    );
    // A numbered option carries the same marker and is not a composer.
    assert_not_rule("codex", &["› 1. Update now"], "composer");
}

#[test]
fn a_numbered_menu_is_a_blocker() {
    let menu = [
        "  ✨ Update available! 0.153.4 -> 0.154.0",
        "› 1. Update now",
        "  2. Skip",
        "  3. Skip until next version",
    ];
    assert_rule("codex", &menu, "numbered_menu", Blocked);
    // One numbered line is ordinary output.
    assert_not_rule("codex", &["› 1. Update now"], "numbered_menu");
    // A list without the marker is an answer, with the composer under it.
    let answer = with_composer(&["• Done. Two changes:", "  1. the parser", "  2. a test", ""]);
    assert_not_rule("codex", &answer, "numbered_menu");
    // The same menu quoted in an answer has the composer row under it.
    let quoted = with_composer(&["› 1. Update now", "  2. Skip", ""]);
    assert_not_rule("codex", &quoted, "numbered_menu");
    assert_no_blocker("codex", &quoted);
    // A numbered list typed into the composer, at rest and while a turn runs.
    for screen in [
        vec![
            "• Done, the parser is split.",
            "",
            "› 1. fix the failing tests",
            "  2. update the docs",
            "",
            "  ? for shortcuts    100% context left",
        ],
        vec![
            "• Working (3s • esc to interrupt)",
            "",
            "› 1. fix the failing tests",
            "  2. update the docs",
            "",
            "  tab to queue message    100% context left",
        ],
        vec![
            "› 1. fix the failing tests",
            "  2. update the docs",
            "",
            "  gpt-5 default · /tmp/project",
        ],
    ] {
        assert_not_rule("codex", &screen, "numbered_menu");
        assert_no_blocker("codex", &screen);
    }
}

#[test]
fn the_shortcuts_footer_is_idle() {
    assert_rule(
        "codex",
        // A composer taller than the bottom rows read for it: only the footer says idle.
        &[
            "› refactor the parser and then",
            "  run every test in the crate",
            "  and the clippy pass after that",
            "",
            "  ? for shortcuts      100% context left",
        ],
        "footer_shortcuts",
        Idle,
    );
    assert_not_rule(
        "codex",
        &["  ? for shortcuts", "› typed"],
        "footer_shortcuts",
    );
}

#[test]
fn the_transcript_pager_and_a_picker_leave_the_reading_alone() {
    assert_skip(
        "codex",
        &[
            "/ / / / / T R A N S C R I P T / / / / /",
            "• an answer",
            "↑/↓ to scroll",
        ],
        "transcript_pager",
    );
    assert_skip(
        "codex",
        &[
            "  Select Approval Mode",
            "› 1. Read Only",
            "  2. Auto",
            "",
            "  Press enter to confirm or esc to go back",
        ],
        "picker_open",
    );
    assert_not_rule("codex", EXEC_APPROVAL, "picker_open");
}

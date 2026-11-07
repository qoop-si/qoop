//! Claude Code, screens built from the strings in its 2.1.284 executable: the composer
//! between two `─` rules with the footer under them, and every dialog under one rule of its
//! own in the composer's place.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const RULE: &str = "────────────────────────────────────────────────────────────";

/// The composer at rest, empty, with its footer.
const COMPOSER: &[&str] = &[RULE, "❯ Try \"fix lint errors\"", RULE, "  ? for shortcuts"];

/// Some transcript, then the composer at rest.
fn idle_under(above: &[&'static str]) -> Vec<&'static str> {
    above.iter().chain(COMPOSER).copied().collect()
}

/// Some transcript, then a dialog under its own rule in the composer's place.
fn dialog(lines: &[&'static str]) -> Vec<&'static str> {
    let mut screen = vec!["● I will remove the old build.", "", RULE];
    screen.extend(lines);
    screen
}

const BASH_DIALOG: &[&str] = &[
    " Bash command",
    "",
    "   rm -rf build",
    "   Remove the old build directory",
    "",
    " Do you want to proceed?",
    " ❯ 1. Yes",
    "   2. Yes, and don't ask again for rm commands in /home/me/project",
    "   3. No",
    "",
    " Esc to cancel · Tab to amend · ctrl+e to explain",
];

#[test]
fn a_bash_permission_is_a_blocker() {
    assert_rule("claude", &dialog(BASH_DIALOG), "permission_dialog", Blocked);
}

#[test]
fn an_edit_and_a_network_permission_are_blockers() {
    assert_rule(
        "claude",
        &dialog(&[
            " Edit file",
            " src/main.rs",
            "   12 -    let x = 1;",
            "   12 +    let x = 2;",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, auto-accept edits",
            "   3. No",
        ]),
        "permission_dialog",
        Blocked,
    );
    assert_rule(
        "claude",
        &dialog(&[
            " Network request outside of sandbox",
            " Host: example.com",
            " Do you want to allow this connection?",
            " > 1. Yes",
            "   2. Yes, and don't ask again for example.com",
            "   3. No, and tell Claude what to do differently (esc)",
        ]),
        "permission_dialog",
        Blocked,
    );
}

#[test]
fn a_permission_quoted_in_an_answer_is_not_a_blocker() {
    let screen = idle_under(&[
        "● The prompt looked like this:",
        "  Do you want to proceed?",
        "  ❯ 1. Yes",
        "    2. No",
        "",
    ]);
    assert_not_rule("claude", &screen, "permission_dialog");
    assert_no_blocker("claude", &screen);
}

/// Claude's own warning on a Bash permission, as it draws it: the command box, the warning in
/// it, often only two options.
const DANGEROUS_RM_DIALOG: &[&str] = &[
    " Bash command",
    " Run shell command",
    "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌",
    " │ rm -f \"$OUT\"/snapshots/*.png",
    "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌",
    " │ Dangerous rm operation on possibly-empty variable path: \"$OUT\"/snapshots/*.png",
    "",
    " Do you want to proceed?",
    " ❯ 1. Yes",
    "   2. No",
    "",
    " Esc to cancel · Tab to amend",
];

#[test]
fn a_dangerous_rm_claude_flagged_is_its_own_blocker() {
    assert_rule(
        "claude",
        &dialog(DANGEROUS_RM_DIALOG),
        "dangerous_rm_dialog",
        Blocked,
    );
    let rmdir: Vec<&str> = DANGEROUS_RM_DIALOG
        .iter()
        .map(|l| {
            if l.contains("Dangerous") {
                " │ Dangerous rmdir operation on statically-unresolvable target"
            } else {
                l
            }
        })
        .collect();
    assert_rule("claude", &dialog(&rmdir), "dangerous_rm_dialog", Blocked);
}

#[test]
fn a_plain_rm_permission_and_a_quoted_warning_are_not_dangerous_rm() {
    assert_not_rule("claude", &dialog(BASH_DIALOG), "dangerous_rm_dialog");
    let quoted = idle_under(&[
        "● Claude showed: Dangerous rm operation on possibly-empty variable path",
        "  ❯ 1. Yes",
        "    2. No",
        "",
    ]);
    assert_not_rule("claude", &quoted, "dangerous_rm_dialog");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn plan_approval_is_a_blocker() {
    let screen = dialog(&[
        " Ready to code?",
        "",
        " Here is Claude's plan:",
        " 1. Split the parser.",
        "",
        " Claude has written up a plan and is ready to execute. Would you like to proceed?",
        "",
        " ❯ 1. Yes, auto-accept edits",
        "   2. Yes, manually approve edits",
        "   3. No, keep planning",
    ]);
    assert_rule("claude", &screen, "plan_approval", Blocked);
    let quoted = idle_under(&[
        "● Next time it asks Ready to code? pick 1.",
        "  Ready to code?",
        "  1. Yes, auto-accept edits",
        "",
    ]);
    assert_not_rule("claude", &quoted, "plan_approval");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn a_plan_mode_request_is_a_blocker() {
    assert_rule(
        "claude",
        &dialog(&[
            " Enter plan mode?",
            " Claude Code wants to enter plan mode",
            " ❯ Yes, enter plan mode",
            "   No, start implementing now",
        ]),
        "plan_mode_request",
        Blocked,
    );
    assert_rule(
        "claude",
        &dialog(&[" Exit plan mode?", " Claude wants to exit plan mode"]),
        "plan_mode_request",
        Blocked,
    );
    let quoted = idle_under(&[
        "● It will ask: Enter plan mode?",
        "  Enter plan mode?",
        "  Claude Code wants to enter plan mode",
        "",
    ]);
    assert_not_rule("claude", &quoted, "plan_mode_request");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn a_question_is_a_blocker() {
    assert_rule(
        "claude",
        &dialog(&[
            " Which database should I use?",
            " ❯ 1. Postgres",
            "   2. SQLite",
            "   3. Type something…",
            "   Chat about this",
            " Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
        ]),
        "selection_prompt",
        Blocked,
    );
    assert_rule(
        "claude",
        &dialog(&[
            " Review your answers",
            " Ready to submit your answers?",
            " ❯ Submit answers",
        ]),
        "question_dialog",
        Blocked,
    );
    let quoted = idle_under(&[
        "● The review screen reads:",
        "  Ready to submit your answers?",
        "  ❯ Submit answers",
        "  Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
        "",
    ]);
    assert_not_rule("claude", &quoted, "question_dialog");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn the_trust_dialog_is_a_setup_gate() {
    let screen = [
        " Claude Code v2.1.284",
        RULE,
        " Accessing workspace:",
        " /home/me/project",
        "",
        " Quick safety check: Is this a project you created or one you trust?",
        "",
        " ❯ No, exit",
        "   Yes, I trust this folder",
    ];
    assert_rule("claude", &screen, "trust_folder", Blocked);
    assert!(verdict("claude", &screen).is_some_and(|v| v.setup_gate));
    let quoted = idle_under(&[
        "● It showed Accessing workspace: and I chose Yes, I trust this folder.",
        "  Accessing workspace:",
        "  Yes, I trust this folder",
        "",
    ]);
    assert_not_rule("claude", &quoted, "trust_folder");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn the_first_run_dialogs_are_setup_gates() {
    let screens: [&[&str]; 4] = [
        &[
            " Let's get started.",
            " Choose the text style that looks best with your terminal",
            " To change this later, run /theme",
            "     Auto (match terminal)",
            " ❯ ✔ Dark mode",
            "     Light mode",
        ],
        &[
            " Detected a custom API key in your environment",
            " ANTHROPIC_API_KEY: sk-ant-...abcd",
            " Do you want to use this API key?",
            "   Yes",
            " ❯ No (recommended)",
        ],
        &[
            " Security notes:",
            " 1. Claude can make mistakes.",
            " Press Enter to continue…",
        ],
        &[
            " WARNING: Claude Code running in Bypass Permissions mode",
            " In Bypass Permissions mode, Claude Code will not ask for your approval",
            " ❯ No, exit",
            "   Yes, I accept",
        ],
    ];
    for screen in screens {
        assert_rule("claude", screen, "first_run_dialog", Blocked);
        assert!(verdict("claude", screen).is_some_and(|v| v.setup_gate));
    }
    // The same picker opened later with /theme is the person's own and stays unflagged.
    assert_not_rule(
        "claude",
        &[
            " Choose the text style that looks best with your terminal",
            " ❯ ✔ Dark mode",
            "   Light mode",
        ],
        "first_run_dialog",
    );
    let quoted = idle_under(&[
        "● On first start it asks:",
        "  Security notes:",
        "  Press Enter to continue…",
        "",
    ]);
    assert_not_rule("claude", &quoted, "first_run_dialog");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn the_login_choice_is_a_setup_gate() {
    let screen = [
        " Claude Code can be used with your Claude subscription or billed based on API usage",
        " Select login method:",
        " ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise",
        "   2. Anthropic Console account · API usage billing",
    ];
    assert_rule("claude", &screen, "login_method", Blocked);
    assert!(verdict("claude", &screen).is_some_and(|v| v.setup_gate));
    assert_not_rule(
        "claude",
        &idle_under(&["● Run /login and pick a login method.", ""]),
        "login_method",
    );
    let quoted = idle_under(&[
        "● On first start it asks:",
        "  Select login method:",
        "  1. Claude account with subscription · Pro, Max, Team, or Enterprise",
        "  2. Anthropic Console account · API usage billing",
        "",
    ]);
    assert_not_rule("claude", &quoted, "login_method");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn the_transcript_viewer_with_a_dialog_behind_it_is_a_blocker() {
    let screen = [
        "● an answer",
        "  dialog waiting · Showing detailed transcript · ctrl+o to toggle · ? for shortcuts",
    ];
    assert_rule("claude", &screen, "transcript_dialog_waiting", Blocked);
    assert_not_rule(
        "claude",
        &[
            "● an answer",
            "  Showing detailed transcript · ctrl+o to toggle · ? for shortcuts",
        ],
        "transcript_dialog_waiting",
    );
}

#[test]
fn the_transcript_viewer_leaves_the_reading_alone() {
    assert_skip(
        "claude",
        &[
            "● an answer",
            "  Showing detailed transcript · ctrl+o to toggle · ↑↓ scroll · v to open in editor · ? for shortcuts",
        ],
        "transcript_viewer",
    );
    assert_not_rule("claude", COMPOSER, "transcript_viewer");
}

#[test]
fn the_model_picker_and_menus_leave_the_reading_alone() {
    assert_skip(
        "claude",
        &[
            RULE,
            " Select model",
            " Switch between Claude models. Your pick becomes the default for new sessions.",
            " ❯ 1. Default (recommended)",
            "   2. Opus",
        ],
        "model_picker",
    );
    assert_not_rule("claude", COMPOSER, "model_picker");
    // The picker's words quoted in an answer leave the reading to the composer, and do not
    // swallow a live dialog drawn under them.
    let quoted = idle_under(&[
        "● /model shows:",
        "  Select model",
        "  Switch between Claude models. Your pick becomes the default for new sessions.",
        "",
    ]);
    assert_not_rule("claude", &quoted, "model_picker");
    let mut with_dialog = vec![
        "● /model shows:",
        "  Select model",
        "  Switch between Claude models. Your pick becomes the default for new sessions.",
    ];
    with_dialog.extend(dialog(BASH_DIALOG));
    assert_rule("claude", &with_dialog, "permission_dialog", Blocked);
    assert_skip(
        "claude",
        &[
            " Permissions",
            " ❯ Allow",
            "   ↑/↓ to navigate · Enter to select · ←/→ to switch · Esc to cancel",
        ],
        "menu_open",
    );
    assert_not_rule("claude", COMPOSER, "menu_open");
}

#[test]
fn the_spinner_row_is_working() {
    for row in [
        "✻ Cogitating…",
        "✶ Pondering… (12s · ↓ 1.2k tokens · still thinking)",
        "· Cooking… (3s)",
        "* Zigzagging…",
        "✻ Beboppin'…",
        "✶ Topsy-turvying… (12s · ↓ 300 tokens)",
        "✻ Flambéing…",
        "✶ Running the test suite… (2m 3s · ↓ 4.1k tokens)",
    ] {
        assert_rule(
            "claude",
            &[
                "● reading files",
                "",
                row,
                "",
                RULE,
                "❯ ",
                RULE,
                "  ? for shortcuts",
            ],
            "spinner_row",
            Working,
        );
    }
    assert_not_rule(
        "claude",
        &idle_under(&["● Done.", "", "✻ Cooked for 1m 3s", ""]),
        "spinner_row",
    );
}

#[test]
fn a_retry_row_is_working() {
    assert_rule(
        "claude",
        &idle_under(&[
            "✻ Waiting for API response · will retry in 4s · check your network",
            "",
        ]),
        "spinner_row",
        Working,
    );
    assert_rule(
        "claude",
        &idle_under(&[
            "✻ Overloaded · next try in 10s · attempt 2 · esc to interrupt",
            "",
        ]),
        "spinner_row",
        Working,
    );
}

#[test]
fn the_busy_footer_is_working() {
    let screen = [
        "● reading files",
        RULE,
        "❯ ",
        RULE,
        "  esc to interrupt · ctrl+t to show tasks",
    ];
    assert_rule("claude", &screen, "footer_interrupt", Working);
    assert_rule(
        "claude",
        &[
            RULE,
            "❯ also fix the docs",
            RULE,
            "  enter to interrupt and send · ctrl+x then enter to queue",
        ],
        "footer_interrupt",
        Working,
    );
    assert_not_rule(
        "claude",
        &idle_under(&["● Press esc to interrupt a turn.", ""]),
        "footer_interrupt",
    );
}

#[test]
fn the_busy_title_is_working() {
    assert_rule_titled("claude", &[""], "◐ Fix the parser", "title_busy", Working);
    assert_rule_titled("claude", &[""], "◑ Claude Code", "title_busy", Working);
    assert_not_rule_titled("claude", &[""], "✳ Fix the parser", "title_busy");
}

#[test]
fn the_empty_composer_is_idle() {
    assert_rule(
        "claude",
        &idle_under(&["● Done.", ""]),
        "composer_empty",
        Idle,
    );
    assert_rule(
        "claude",
        &["● Done.", RULE, "❯ ", RULE, "  ? for shortcuts"],
        "composer_empty",
        Idle,
    );
    assert_rule(
        "claude",
        &[RULE, "!", RULE, "  ! for shell mode"],
        "composer_empty",
        Idle,
    );
    // A rule in the transcript is not a composer.
    assert_not_rule(
        "claude",
        &["● table:", RULE, "  a | b", RULE, "● done"],
        "composer_empty",
    );
    assert_not_rule(
        "claude",
        &["● Type ? for shortcuts any time."],
        "footer_shortcuts",
    );
}

/// A draft right under the end of a turn: the turn's summary or the interrupt note, with
/// only indented notices between it and the composer.
#[test]
fn a_draft_under_the_end_of_a_turn_is_idle() {
    let footer = "  ⏵⏵ accept edits on (shift+tab to cycle)";
    assert_rule(
        "claude",
        &[
            "✻ Crunched for 16m 31s · done 6:31 PM",
            "                              You've used 75% of your weekly limit",
            RULE,
            "❯ close them all",
            RULE,
            footer,
        ],
        "draft_after_turn",
        Idle,
    );
    assert_rule(
        "claude",
        &[
            "  ⎿  Interrupted · What should Claude do instead?",
            "                    You've used 75% of your weekly limit",
            RULE,
            "❯ Write a 1500 word essay about pianos.",
            RULE,
            footer,
        ],
        "draft_after_turn",
        Idle,
    );
    // Every summary verb counts, accented ones too.
    assert_rule(
        "claude",
        &[
            "✻ Sautéed for 2m 3s · done 6:31 PM",
            RULE,
            "❯ next",
            RULE,
            footer,
        ],
        "draft_after_turn",
        Idle,
    );
    // Claude compacts an idle session before its prompt cache expires and says so; its
    // suggested next prompt sits in the composer.
    assert_rule(
        "claude",
        &[
            "  ⎿  Skills restored (commit)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired · turn off with \"idleCompaction\": false in settings.json",
            "",
            RULE,
            "❯ deploy it, then try it live",
            RULE,
            footer,
        ],
        "draft_after_turn",
        Idle,
    );
    // A narrower pane wraps that note, its tail indented two columns.
    assert_rule(
        "claude",
        &[
            "  ⎿  Skills restored (commit, ui-ux, market-research)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired · turn off with \"idleCompaction\": false in ",
            "  settings.json",
            "",
            RULE,
            "❯ push it and open a PR",
            RULE,
            footer,
        ],
        "draft_after_turn",
        Idle,
    );
    // A prompt just sent sits between the old summary and the composer.

    assert_not_rule(
        "claude",
        &[
            "✻ Brewed for 32s · done 6:46 PM",
            "❯ Write a 1500 word essay about pianos.",
            RULE,
            "❯ draft",
            RULE,
            footer,
        ],
        "draft_after_turn",
    );
}

/// Any other draft is a turn that may still be running: weak working, never visible idle.
#[test]
fn a_draft_alone_is_not_visible_idle() {
    let screen = [
        "  collisions and let typists work faster without jams.",
        "                         You've used 75% of your weekly limit",
        RULE,
        "❯ draft",
        RULE,
        "  ⏵⏵ accept edits on (shift+tab to cycle)",
    ];
    let v = verdict("claude", &screen).expect("a verdict");
    assert_eq!(v.rule, "composer_draft", "{v:?}");
    assert_eq!(v.state, Working);
    assert!(!v.visible_idle && !v.visible_working, "{v:?}");
    assert_not_rule("claude", COMPOSER, "composer_draft");
}

/// A choice put to the person, its hint under the last rule.
#[test]
fn a_selection_prompt_is_a_blocker() {
    assert_rule(
        "claude",
        &[
            "There are 1700 lines not committed yet. What should we do?",
            "  1. Read the state first",
            "❯ 2. Finish the palette work",
            "  3. Something else",
            "Enter to select · ↑/↓ to navigate · Esc to cancel",
        ],
        "selection_prompt",
        Blocked,
    );
    assert_rule(
        "claude",
        &[
            RULE,
            "❯ Sonnet",
            "  Opus",
            RULE,
            " Enter to select · Esc to cancel",
        ],
        "selection_prompt",
        Blocked,
    );
    let quoted = idle_under(&[
        "● The menu says:",
        "  Enter to select · ↑/↓ to navigate · Esc to cancel",
        "",
    ]);
    assert_not_rule("claude", &quoted, "selection_prompt");
    assert_no_blocker("claude", &quoted);
}

#[test]
fn the_shortcuts_footer_is_idle_where_no_composer_box_is_read() {
    // Plan mode's footer, under a composer whose top rule scrolled off.
    assert_rule(
        "claude",
        &["❯ ", RULE, "  ? for shortcuts"],
        "footer_shortcuts",
        Idle,
    );
}

#[test]
fn a_working_turn_is_not_also_idle() {
    let screen = [
        "● reading files",
        "",
        "✻ Cogitating… (5s · ↓ 300 tokens)",
        "",
        RULE,
        "❯ ",
        RULE,
        "  esc to interrupt",
    ];
    let v = verdict("claude", &screen).expect("a verdict");
    assert_eq!(v.state, Working, "{v:?}");
}

#[test]
fn a_dialog_outranks_the_spinner_above_it() {
    let mut screen = vec!["✻ Cogitating… (5s)", ""];
    screen.extend(dialog(BASH_DIALOG));
    let v = verdict("claude", &screen).expect("a verdict");
    assert_eq!(v.rule, "permission_dialog", "{v:?}");
}

#[test]
fn an_mcp_tool_permission_is_a_blocker() {
    assert_rule(
        "claude",
        &dialog(&[
            " Tool use",
            "",
            "   github - create_issue (MCP)",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don't ask again for github - create_issue commands in /home/me/project",
            "   3. No, and tell Claude what to do differently (esc)",
        ]),
        "permission_dialog",
        Blocked,
    );
}

/// A fresh session draws its composer with a dim suggestion in it, after a no-break space as
/// real sessions draw it, or a plain one. Nothing is typed, so the agent is idle at once.
#[test]
fn a_fresh_composer_with_a_suggestion_is_visibly_idle() {
    let rule = "\u{2500}".repeat(78);
    for hint in [
        "\u{276F}\u{a0}Try \"write a test for daemon.rs\"",
        "\u{276F} Try \"write a test for daemon.rs\"",
        "\u{276F} Try \"how does <filepath> work?\"",
        "\u{276F} Try \"fix lint err\u{2026}",
    ] {
        let screen = [
            "\u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.284",
            "",
            &rule,
            hint,
            &rule,
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)",
        ];
        let v = verdict("claude", &screen).expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{hint}: {v:?}");
        assert!(v.visible_idle, "{hint}: {v:?}");
    }
}

/// A typed draft, after either space, is not the suggestion: never a visible idle, even with
/// the `? for shortcuts` footer under it.
#[test]
fn a_draft_that_is_not_the_suggestion_is_not_visibly_idle() {
    let rule = "\u{2500}".repeat(78);
    for draft in [
        "\u{276F} Try again with the other file",
        "\u{276F}\u{a0}fix the parser",
    ] {
        let v =
            verdict("claude", &["", &rule, draft, &rule, "  ? for shortcuts"]).expect("a verdict");
        assert!(!v.visible_idle, "{draft}: {v:?}");
    }
}

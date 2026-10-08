//! pi, screens built from `packages/coding-agent` and `packages/tui`.

use super::*;
use AgentRuntimeState::{Blocked, Idle, Working};

const BORDER: &str = "────────────────────────────────────────────────────────────";
const FOOTER: &[&str] = &[
    "~/project (main)",
    "↑1.2k ↓3.4k $0.123 12.3%/200k (auto)                  claude-sonnet-4 • thinking off",
];

fn below(block: &[&'static str]) -> Vec<&'static str> {
    block.iter().chain(FOOTER).copied().collect()
}

/// An idle editor under some transcript: two plain borders and the footer.
fn editor_under(above: &[&'static str]) -> Vec<&'static str> {
    let mut lines: Vec<&str> = above.to_vec();
    lines.extend([BORDER, "", BORDER]);
    lines.extend(FOOTER);
    lines
}

#[test]
fn an_extension_confirm_is_a_blocker() {
    let screen = below(&[
        " Done writing the file.",
        BORDER,
        "",
        " Session cwd not found",
        " The session's directory no longer exists. Continue here?",
        "",
        " → Yes",
        "   No",
        "",
        " ↑↓ navigate  enter select  escape cancel",
        "",
        BORDER,
    ]);
    assert_rule("pi", &screen, "confirm_dialog", Blocked);
}

#[test]
fn a_confirm_quoted_in_an_answer_is_not_a_blocker() {
    let screen = editor_under(&[
        " The dialog looks like this:",
        " → Yes",
        "   No",
        " ↑↓ navigate  enter select  escape cancel",
    ]);
    assert_not_rule("pi", &screen, "confirm_dialog");
    assert_no_blocker("pi", &screen);
    let typed = {
        let mut lines = vec![
            " → Yes",
            "   No",
            " ↑↓ navigate  enter select  escape cancel",
        ];
        lines.extend([BORDER, "and then?", BORDER]);
        lines.extend(FOOTER);
        lines
    };
    assert_no_blocker("pi", &typed);
}

#[test]
fn project_trust_is_a_setup_gate() {
    let screen = [
        " π v1.0.4",
        BORDER,
        " Project trust",
        " ~/project",
        " Saved decision: none",
        " Current session: untrusted",
        " →   Trust",
        "     Trust parent folder (~/code)",
        "     Do not trust",
        " ↑↓ navigate  enter save  escape cancel",
        BORDER,
    ];
    assert_rule("pi", &screen, "project_trust", Blocked);
    assert!(verdict("pi", &screen).is_some_and(|v| v.setup_gate));
    let quoted = editor_under(&[
        " It shows this on the first start:",
        " Project trust",
        " ↑↓ navigate  enter save  escape cancel",
    ]);
    assert_not_rule("pi", &quoted, "project_trust");
    assert_no_blocker("pi", &quoted);
}

#[test]
fn a_select_dialog_is_a_blocker() {
    // An extension's `ui.select("Which database?", ["Postgres", "SQLite"])`.
    let question = below(&[
        BORDER,
        " Which database?",
        " → Postgres",
        "   SQLite",
        " ↑↓ navigate  enter select  escape cancel",
        BORDER,
    ]);
    assert_rule("pi", &question, "select_dialog", Blocked);
    // pi's own model selector reads the same.
    let model = below(&[
        BORDER,
        " Select model",
        " → claude-sonnet-4",
        "   gpt-5",
        " ↑↓ navigate  enter select  escape cancel",
        BORDER,
    ]);
    assert_rule("pi", &model, "select_dialog", Blocked);
    assert_not_rule("pi", &editor_under(&[" an answer"]), "select_dialog");
    let quoted = editor_under(&[
        " Which database?",
        " → Postgres",
        "   SQLite",
        " ↑↓ navigate  enter select  escape cancel",
    ]);
    assert_not_rule("pi", &quoted, "select_dialog");
    assert_no_blocker("pi", &quoted);
}

#[test]
fn the_working_border_is_working() {
    for top in [
        "── ⠋ Working ───────────────────────────────────────────────",
        "── ⠸ Working (escape to interrupt) ─────────────────────────",
        "── ⠼ Compacting context... (escape to cancel) ──────────────",
    ] {
        let mut screen = vec![" reading files", top, ""];
        screen.push(BORDER);
        screen.extend(FOOTER);
        assert_rule("pi", &screen, "editor_border_working", Working);
    }
    assert_not_rule(
        "pi",
        &editor_under(&[" an answer"]),
        "editor_border_working",
    );
}

#[test]
fn the_editor_at_rest_is_idle() {
    assert_rule("pi", &editor_under(&[" an answer"]), "editor_idle", Idle);
    let mut typed = vec![" an answer", BORDER, "fix the", "parser please", BORDER];
    typed.extend(FOOTER);
    assert_rule("pi", &typed, "editor_idle", Idle);
    let mut working = vec![
        " an answer",
        "── ⠋ Working ───────────────────────────────────────────────",
        "",
        BORDER,
    ];
    working.extend(FOOTER);
    assert_not_rule("pi", &working, "editor_idle");
}

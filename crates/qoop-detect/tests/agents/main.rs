//! Screen rules for each agent, read against small screens written from the exact text in
//! that agent's own source code and documentation.
//!
//! Every rule has a screen it must match and a screen it must not. For a rule that raises
//! a visible blocker the screen it must not match is the same words inside an answer the
//! agent gave, with its idle prompt below: a blocker outranks every weaker source under
//! ADR-008, so a phrase quoted in the transcript that raised one would pin an idle agent
//! as waiting on a person.

use qoop_detect::{OscContext, Reading, Verdict, for_program};
use qoop_domain::AgentRuntimeState;

mod amp;
mod antigravity;
mod claude;
mod cline;
mod codex;
mod copilot;
mod cursor;
mod devin;
mod droid;
mod gemini;
mod grok;
mod hermes;
mod kilo;
mod kimi;
mod kiro;
mod maki;
mod muse;
mod opencode;
mod pi;
mod qodercli;
mod qwen;

/// The bundled verdict for a screen given as lines, with no title.
pub fn verdict(agent: &str, lines: &[&str]) -> Option<Verdict> {
    verdict_titled(agent, lines, "")
}

/// The bundled verdict for a screen and an OSC title.
pub fn verdict_titled(agent: &str, lines: &[&str], title: &str) -> Option<Verdict> {
    let screen: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
    for_program(agent)
        .unwrap_or_else(|| panic!("{agent} has a bundled manifest"))
        .evaluate_with_osc(&screen, &OscContext { title })
}

/// Assert that `rule` wins on this screen with this state.
#[track_caller]
pub fn assert_rule(agent: &str, lines: &[&str], rule: &str, state: AgentRuntimeState) {
    assert_rule_titled(agent, lines, "", rule, state);
}

/// The same, with an OSC title.
#[track_caller]
pub fn assert_rule_titled(
    agent: &str,
    lines: &[&str],
    title: &str,
    rule: &str,
    state: AgentRuntimeState,
) {
    let v = verdict_titled(agent, lines, title)
        .unwrap_or_else(|| panic!("{agent}: nothing matched, expected {rule}: {lines:?}"));
    assert_eq!(v.rule, rule, "{agent}: {lines:?}");
    assert_eq!(v.state, state, "{agent}: {v:?}");
    match state {
        AgentRuntimeState::Blocked => assert!(v.visible_blocker || rule.contains("legacy")),
        AgentRuntimeState::Working => assert!(v.visible_working, "{v:?}"),
        _ => {}
    }
}

/// Assert that `rule` does not win on this screen.
#[track_caller]
pub fn assert_not_rule(agent: &str, lines: &[&str], rule: &str) {
    assert_not_rule_titled(agent, lines, "", rule);
}

/// The same, with an OSC title. A `skip_state_update` rule winning counts as a match too:
/// the verdict is `None` then, and reading only the verdict would let a wrong skip pass.
#[track_caller]
pub fn assert_not_rule_titled(agent: &str, lines: &[&str], title: &str, rule: &str) {
    let screen: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
    let reading = for_program(agent)
        .unwrap_or_else(|| panic!("{agent} has a bundled manifest"))
        .read_with_osc(&screen, &OscContext { title });
    let won = match &reading {
        Reading::Verdict(v) => v.rule == rule,
        Reading::Skip { rule: skipped } => skipped == rule,
        Reading::Nothing => false,
    };
    assert!(
        !won,
        "{agent}: {rule} matched a screen it must not: {reading:?} on {lines:?}"
    );
}

/// Assert that nothing on this screen reads as a visible blocker.
#[track_caller]
pub fn assert_no_blocker(agent: &str, lines: &[&str]) {
    let v = verdict(agent, lines);
    assert!(
        !v.as_ref().is_some_and(|v| v.visible_blocker),
        "{agent}: a blocker was read out of an answer: {v:?} on {lines:?}"
    );
}

/// Assert that `rule` wins as a `skip_state_update` rule: the screen is a view the person
/// opened, and the last reading stands.
#[track_caller]
pub fn assert_skip(agent: &str, lines: &[&str], rule: &str) {
    let screen: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
    let reading = for_program(agent)
        .unwrap_or_else(|| panic!("{agent} has a bundled manifest"))
        .read_with_osc(&screen, &OscContext { title: "" });
    assert_eq!(
        reading,
        Reading::Skip {
            rule: rule.to_string()
        },
        "{agent}: {lines:?}"
    );
}

/// Agents without a long-proven resume path come back only into a conversation whose id was
/// reported, never into whichever one is newest.
#[test]
fn agents_without_a_proven_resume_have_no_latest_line() {
    for agent in [
        "devin",
        "kiro",
        "kimi",
        "kilo",
        "hermes",
        "antigravity",
        "pi",
        "qodercli",
        "muse",
        "maki",
        "cline",
    ] {
        assert_eq!(
            qoop_detect::resume_argv(agent, None),
            None,
            "{agent} has a latest line"
        );
        assert!(qoop_detect::resumes_by_id(agent), "{agent} resumes by id");
    }
}

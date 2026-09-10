//! What the screen says an agent is doing.
//!
//! `qoro` knows which program runs in a terminal from the process table, and that is a fact.
//! It does not know whether that program is thinking, waiting for an answer, or finished,
//! because no such fact exists outside the program. Until an agent reports its own state
//! over a protocol, the only evidence available is the pixels it drew, so this crate reads
//! them.
//!
//! Two decisions shape the whole crate. Rules live in TOML manifests rather than in Rust,
//! because a spinner glyph changes in a patch release of somebody else's tool and shipping a
//! new binary for that is absurd. And this crate returns evidence, never a decision: the
//! precedence rules in ADR-008 weigh a screen reading against a hook and a native protocol,
//! and a scraper that believed itself would overrule sources that actually know.
//!
//! The engine is pure. Screen text in, a [`Verdict`] out, no I/O and no clock.
//!
//! There is a manifest with the id `generic` for programs that have no manifest of their
//! own. It is reachable through [`for_program`] like any other, so a caller that wants the
//! fallback asks for it by name.

#![forbid(unsafe_code)]

mod region;

use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use qoro_domain::AgentRuntimeState;

pub use region::Region;
use region::RegionView;

/// The most screen lines any rule may look at.
///
/// Matching cost is linear in the input and a rule set runs on every screen poll, so an
/// uncapped region makes the cost of watching a terminal proportional to how long it has
/// been open. The live prompt is at the bottom by construction, and a dialog tall enough to
/// need more than two hundred lines of context is a dialog nobody can read either.
pub const MAX_REGION_LINES: usize = 200;

/// A parsed and compiled rule set for one agent.
///
/// Compiled means every regex in it is already valid: a manifest that parses cannot fail
/// later during matching, which is the point of doing the work once at startup.
#[derive(Debug)]
pub struct Manifest {
    /// The program this rule set describes, for example `claude`.
    pub id: String,
    /// The rule set's own version, so a wrong answer can be traced to the rules that gave
    /// it rather than to the engine.
    pub version: String,
    rules: Vec<Rule>,
}

/// What the screen says, when it says anything.
///
/// The `visible_*` flags are not decoration. ADR-008 lets a screen reading override a
/// stronger source in exactly one case, a blocker that is positively on screen, and
/// `visible_blocker` is how this crate makes that claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// The state the winning rule declares.
    pub state: AgentRuntimeState,
    /// The id of the rule that matched. A wrong answer is somebody's afternoon unless the
    /// answer says which line of which manifest produced it.
    pub rule: String,
    /// A blocking prompt is positively on screen.
    pub visible_blocker: bool,
    /// An idle prompt is positively on screen.
    pub visible_idle: bool,
    /// Work in progress is positively on screen.
    pub visible_working: bool,
}

/// Why a manifest was rejected.
///
/// Every variant names the rule it came from. A manifest is edited by a person under time
/// pressure while an agent is misreporting, and "invalid regex" without a rule id is a
/// second problem on top of the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// The file is not valid TOML, or does not match the schema.
    Toml(String),
    /// A regex in a rule does not compile. Caught here so it can never panic at match time.
    BadRegex {
        rule: String,
        pattern: String,
        message: String,
    },
    /// A rule asks for a region this engine does not have.
    UnknownRegion { rule: String, region: String },
    /// A rule declares a state that is not an `AgentRuntimeState`.
    UnknownState { rule: String, state: String },
    /// A rule has no leaf test and no gate, so it would match every screen. Almost always a
    /// misspelled key, and at a high priority it would mask every rule below it.
    EmptyRule { rule: String },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(message) => write!(f, "manifest is not valid: {message}"),
            Self::BadRegex {
                rule,
                pattern,
                message,
            } => write!(
                f,
                "rule {rule}: pattern {pattern:?} does not compile: {message}"
            ),
            Self::UnknownRegion { rule, region } => {
                write!(f, "rule {rule}: unknown region {region:?}")
            }
            Self::UnknownState { rule, state } => {
                write!(f, "rule {rule}: unknown state {state:?}")
            }
            Self::EmptyRule { rule } => {
                write!(f, "rule {rule}: no test, so it would match every screen")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// One compiled rule.
#[derive(Debug)]
struct Rule {
    id: String,
    state: AgentRuntimeState,
    priority: i32,
    region: Region,
    visible_blocker: bool,
    visible_idle: bool,
    visible_working: bool,
    skip_state_update: bool,
    gate: Gate,
}

/// A compiled gate.
///
/// Every part that is present must hold, so a rule that names both a `contains` and an `any`
/// means both, which is the shape the manifests are written in. `any` holds when one branch
/// holds and `not` holds when no branch does.
#[derive(Debug, Default)]
struct Gate {
    contains: Option<String>,
    regex: Option<Regex>,
    line_regex: Option<Regex>,
    all: Vec<Gate>,
    any: Vec<Gate>,
    not: Vec<Gate>,
}

impl Gate {
    fn matches(&self, view: &RegionView<'_>) -> bool {
        if let Some(needle) = &self.contains
            && !view.lower.contains(needle.as_str())
        {
            return false;
        }
        if let Some(re) = &self.regex
            && !re.is_match(&view.text)
        {
            return false;
        }
        if let Some(re) = &self.line_regex
            && !view.lines.iter().any(|line| re.is_match(line))
        {
            return false;
        }
        if !self.all.iter().all(|g| g.matches(view)) {
            return false;
        }
        if !self.any.is_empty() && !self.any.iter().any(|g| g.matches(view)) {
            return false;
        }
        if self.not.iter().any(|g| g.matches(view)) {
            return false;
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.contains.is_none()
            && self.regex.is_none()
            && self.line_regex.is_none()
            && self.all.is_empty()
            && self.any.is_empty()
            && self.not.is_empty()
    }
}

/// The on disk schema. Kept separate from the compiled form so `deny_unknown_fields` can do
/// its job: a misspelled `line_regexp` that serde skipped would be a rule that never fires
/// and never complains.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    version: String,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    state: String,
    #[serde(default)]
    priority: i32,
    #[serde(default = "whole_screen")]
    region: String,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    // The gate is spelled out here rather than flattened, because serde's `flatten` and
    // `deny_unknown_fields` cannot both apply to the same struct.
    contains: Option<String>,
    regex: Option<String>,
    line_regex: Option<String>,
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default)]
    not: Vec<RawGate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    contains: Option<String>,
    regex: Option<String>,
    line_regex: Option<String>,
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default)]
    not: Vec<RawGate>,
}

fn whole_screen() -> String {
    "whole_screen".to_string()
}

fn compile(pattern: &str, rule: &str) -> Result<Regex, ManifestError> {
    Regex::new(pattern).map_err(|e| ManifestError::BadRegex {
        rule: rule.to_string(),
        pattern: pattern.to_string(),
        message: e.to_string(),
    })
}

impl RawGate {
    fn compile(self, rule: &str) -> Result<Gate, ManifestError> {
        Ok(Gate {
            contains: self.contains.map(|s| s.to_lowercase()),
            regex: self.regex.map(|p| compile(&p, rule)).transpose()?,
            line_regex: self.line_regex.map(|p| compile(&p, rule)).transpose()?,
            all: compile_all(self.all, rule)?,
            any: compile_all(self.any, rule)?,
            not: compile_all(self.not, rule)?,
        })
    }
}

fn compile_all(gates: Vec<RawGate>, rule: &str) -> Result<Vec<Gate>, ManifestError> {
    gates.into_iter().map(|g| g.compile(rule)).collect()
}

fn parse_state(state: &str, rule: &str) -> Result<AgentRuntimeState, ManifestError> {
    match state {
        "starting" => Ok(AgentRuntimeState::Starting),
        "idle" => Ok(AgentRuntimeState::Idle),
        "working" => Ok(AgentRuntimeState::Working),
        "blocked" => Ok(AgentRuntimeState::Blocked),
        "done" => Ok(AgentRuntimeState::Done),
        "failed" => Ok(AgentRuntimeState::Failed),
        "unknown" => Ok(AgentRuntimeState::Unknown),
        // `Uncertain` is deliberately absent. It means two sources disagree, which is a
        // conclusion the precedence layer draws; a single rule cannot disagree with itself.
        _ => Err(ManifestError::UnknownState {
            rule: rule.to_string(),
            state: state.to_string(),
        }),
    }
}

impl Manifest {
    /// Parse and compile a manifest.
    ///
    /// Everything that could go wrong at match time is checked here instead: an invalid
    /// regex, an unknown region and an unknown state are all parse errors naming the rule.
    /// The alternative is a panic inside a screen poll, which takes the daemon down because
    /// somebody typed a stray bracket in a TOML file.
    pub fn parse(toml_text: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest =
            toml::from_str(toml_text).map_err(|e| ManifestError::Toml(e.to_string()))?;
        let mut rules = Vec::with_capacity(raw.rules.len());
        for r in raw.rules {
            let region = Region::parse(&r.region).ok_or_else(|| ManifestError::UnknownRegion {
                rule: r.id.clone(),
                region: r.region.clone(),
            })?;
            let state = parse_state(&r.state, &r.id)?;
            let gate = RawGate {
                contains: r.contains,
                regex: r.regex,
                line_regex: r.line_regex,
                all: r.all,
                any: r.any,
                not: r.not,
            }
            .compile(&r.id)?;
            if gate.is_empty() {
                return Err(ManifestError::EmptyRule { rule: r.id });
            }
            rules.push(Rule {
                id: r.id,
                state,
                priority: r.priority,
                region,
                visible_blocker: r.visible_blocker,
                visible_idle: r.visible_idle,
                visible_working: r.visible_working,
                skip_state_update: r.skip_state_update,
                gate,
            });
        }
        Ok(Self {
            id: raw.id,
            version: raw.version,
            rules,
        })
    }

    /// The highest priority rule that matches, or `None` when the screen says nothing.
    ///
    /// A tie keeps the earliest declared rule, so a manifest's reading order is also its
    /// tie break and a person editing one can predict the outcome. `None` also comes back
    /// when the winning rule sets `skip_state_update`: see that field in the manifests.
    pub fn evaluate(&self, screen: &[String]) -> Option<Verdict> {
        let capped = &screen[screen.len().saturating_sub(MAX_REGION_LINES)..];
        let mut best: Option<&Rule> = None;
        for rule in &self.rules {
            // Strictly greater, so the first rule declared at a given priority holds it.
            // This also skips the matching work for rules that could not win anyway.
            if best.is_some_and(|b| b.priority >= rule.priority) {
                continue;
            }
            if rule.gate.matches(&rule.region.view(capped)) {
                best = Some(rule);
            }
        }
        let rule = best?;
        if rule.skip_state_update {
            return None;
        }
        Some(Verdict {
            state: rule.state,
            rule: rule.id.clone(),
            visible_blocker: rule.visible_blocker,
            visible_idle: rule.visible_idle,
            visible_working: rule.visible_working,
        })
    }
}

/// The manifests compiled into the binary, as source text.
const SOURCES: &[&str] = &[
    include_str!("../manifests/claude.toml"),
    include_str!("../manifests/codex.toml"),
    include_str!("../manifests/generic.toml"),
];

static BUILTIN: OnceLock<Vec<Manifest>> = OnceLock::new();

/// Every manifest compiled into the binary, parsed once.
///
/// The parse cannot fail in a shipped binary: the input is a compile time constant and
/// `every_builtin_manifest_parses` fails the build if one of them is broken. Panicking here
/// is therefore a statement that the test suite ran, not error handling.
pub fn builtin() -> &'static [Manifest] {
    BUILTIN.get_or_init(|| {
        SOURCES
            .iter()
            .map(|src| Manifest::parse(src).expect("a builtin manifest must parse"))
            .collect()
    })
}

/// The manifest for a program name, if there is one.
///
/// The comparison is case insensitive because the name arrives from a process table. There
/// is no fuzzy matching: an unrecognised program gets `None`, and the caller decides whether
/// to fall back to the `generic` manifest.
pub fn for_program(name: &str) -> Option<&'static Manifest> {
    builtin().iter().find(|m| m.id.eq_ignore_ascii_case(name))
}

/// The rules to use for a program, falling back to the generic ones.
///
/// The fallback is the whole reason a terminal running a plain shell says anything at all.
/// Without it the caller asks for a manifest named `bash`, is told there is none, and the
/// state column stays empty for every terminal that is not one of the three agents this
/// build happens to know: measured, exactly that, on a shell sitting at its own prompt.
pub fn rules_for(name: &str) -> Option<&'static Manifest> {
    for_program(name).or_else(|| for_program("generic"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| (*s).to_string()).collect()
    }

    fn one_rule(body: &str) -> Manifest {
        Manifest::parse(&format!(
            "id = \"t\"\nversion = \"1\"\n\n[[rules]]\nid = \"r\"\nstate = \"blocked\"\n{body}"
        ))
        .expect("the test manifest should parse")
    }

    fn matched(m: &Manifest, lines: &[&str]) -> bool {
        m.evaluate(&screen(lines)).is_some()
    }

    #[test]
    fn a_contains_leaf_ignores_case() {
        let m = one_rule("contains = \"Do You Want To Proceed\"\n");
        assert!(matched(&m, &["do you want to proceed?"]));
        assert!(matched(&m, &["DO YOU WANT TO PROCEED?"]));
        assert!(!matched(&m, &["nothing here"]));
    }

    #[test]
    fn a_regex_leaf_sees_the_whole_region_and_a_line_regex_sees_one_line_at_a_time() {
        let whole = one_rule("regex = 'proceed\\?\\napply'\n");
        assert!(matched(&whole, &["proceed?", "apply"]));
        let per_line = one_rule("line_regex = '^apply$'\n");
        assert!(matched(&per_line, &["proceed?", "apply"]));
        // The same pattern against the joined region would need a multi line flag, which is
        // exactly the distinction the two leaves exist to make.
        assert!(!matched(&one_rule("regex = '^apply$'\n"), &["x", "apply"]));
    }

    #[test]
    fn an_any_gate_matches_when_one_branch_matches() {
        let m = one_rule("any = [ { contains = \"yes\" }, { contains = \"no\" } ]\n");
        assert!(matched(&m, &["no"]));
        assert!(!matched(&m, &["maybe"]));
    }

    #[test]
    fn an_all_gate_needs_every_branch() {
        let m = one_rule("all = [ { contains = \"yes\" }, { contains = \"no\" } ]\n");
        assert!(matched(&m, &["yes", "no"]));
        assert!(!matched(&m, &["yes"]));
    }

    #[test]
    fn a_not_gate_fails_as_soon_as_one_branch_matches() {
        let m = one_rule("contains = \"proceed\"\nnot = [ { contains = \"transcript\" } ]\n");
        assert!(matched(&m, &["proceed?"]));
        assert!(!matched(&m, &["proceed?", "showing detailed transcript"]));
    }

    #[test]
    fn the_parts_of_one_gate_are_combined_with_and() {
        let m = one_rule("contains = \"proceed\"\nany = [ { contains = \"yes\" } ]\n");
        assert!(matched(&m, &["proceed? yes"]));
        assert!(!matched(&m, &["proceed?"]));
        assert!(!matched(&m, &["yes"]));
    }

    #[test]
    fn gates_nest_to_any_depth() {
        let m = one_rule(
            "any = [ { all = [ { contains = \"a\" }, { not = [ { contains = \"b\" } ] } ] }, { contains = \"c\" } ]\n",
        );
        assert!(matched(&m, &["a"]));
        assert!(!matched(&m, &["a b"]));
        assert!(matched(&m, &["a b c"]));
    }

    #[test]
    fn the_highest_priority_matching_rule_wins_whatever_the_declaration_order() {
        let m = Manifest::parse(
            r#"
id = "t"
version = "1"

[[rules]]
id = "low"
state = "idle"
priority = 10
contains = "prompt"

[[rules]]
id = "high"
state = "blocked"
priority = 900
contains = "prompt"
"#,
        )
        .expect("parses");
        let v = m.evaluate(&screen(&["prompt"])).expect("a rule matches");
        assert_eq!(v.rule, "high");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
    }

    #[test]
    fn a_priority_tie_keeps_the_earliest_declared_rule() {
        let m = Manifest::parse(
            r#"
id = "t"
version = "1"

[[rules]]
id = "first"
state = "idle"
priority = 500
contains = "prompt"

[[rules]]
id = "second"
state = "blocked"
priority = 500
contains = "prompt"
"#,
        )
        .expect("parses");
        assert_eq!(
            m.evaluate(&screen(&["prompt"]))
                .expect("a rule matches")
                .rule,
            "first"
        );
    }

    #[test]
    fn a_matching_skip_state_update_rule_reports_nothing_at_all() {
        // A pager or a transcript is a picture of the past. Updating state from it is worse
        // than leaving the last real reading alone, so the answer is `None` rather than a
        // state, and it is `None` even though a lower priority rule did match.
        let m = Manifest::parse(
            r#"
id = "t"
version = "1"

[[rules]]
id = "transcript"
state = "unknown"
priority = 1000
skip_state_update = true
contains = "showing detailed transcript"

[[rules]]
id = "blocker"
state = "blocked"
priority = 900
contains = "do you want to proceed"
"#,
        )
        .expect("parses");
        assert!(
            m.evaluate(&screen(&["do you want to proceed?"]))
                .is_some_and(|v| v.rule == "blocker")
        );
        assert_eq!(
            m.evaluate(&screen(&[
                "do you want to proceed?",
                "showing detailed transcript"
            ])),
            None
        );
    }

    #[test]
    fn a_regex_that_does_not_compile_is_a_parse_error_naming_the_rule() {
        let err = Manifest::parse(
            "id = \"t\"\nversion = \"1\"\n\n[[rules]]\nid = \"broken\"\nstate = \"idle\"\nany = [ { line_regex = '([unclosed' } ]\n",
        )
        .expect_err("an invalid pattern must be rejected at parse time");
        match &err {
            ManifestError::BadRegex { rule, .. } => assert_eq!(rule, "broken"),
            other => panic!("wrong error: {other}"),
        }
        assert!(err.to_string().contains("broken"));
    }

    #[test]
    fn an_unknown_region_is_a_parse_error_naming_the_rule() {
        let err = one_rule_err("region = \"prompt_box_body\"\ncontains = \"x\"\n");
        assert_eq!(
            err,
            ManifestError::UnknownRegion {
                rule: "r".to_string(),
                region: "prompt_box_body".to_string(),
            }
        );
    }

    #[test]
    fn an_unknown_state_is_a_parse_error() {
        let err = Manifest::parse(
            "id = \"t\"\nversion = \"1\"\n\n[[rules]]\nid = \"r\"\nstate = \"busy\"\ncontains = \"x\"\n",
        )
        .expect_err("busy is not a runtime state");
        assert_eq!(
            err,
            ManifestError::UnknownState {
                rule: "r".to_string(),
                state: "busy".to_string(),
            }
        );
    }

    #[test]
    fn a_rule_with_no_test_is_a_parse_error() {
        // Otherwise a misspelled key turns into a rule that matches every screen.
        assert_eq!(
            one_rule_err("priority = 900\n"),
            ManifestError::EmptyRule {
                rule: "r".to_string()
            }
        );
    }

    #[test]
    fn a_misspelled_key_is_a_parse_error_rather_than_a_silently_dead_rule() {
        assert!(matches!(
            one_rule_err("line_regexp = 'x'\n"),
            ManifestError::Toml(_)
        ));
    }

    fn one_rule_err(body: &str) -> ManifestError {
        Manifest::parse(&format!(
            "id = \"t\"\nversion = \"1\"\n\n[[rules]]\nid = \"r\"\nstate = \"blocked\"\n{body}"
        ))
        .expect_err("this manifest should be rejected")
    }

    #[test]
    fn matching_never_looks_further_back_than_the_cap() {
        let m = one_rule("contains = \"ancient\"\n");
        let mut lines = vec!["ancient prompt".to_string()];
        lines.extend((0..MAX_REGION_LINES).map(|i| format!("line {i}")));
        assert_eq!(m.evaluate(&lines), None, "the cap must hide old scrollback");
        // One line shorter and the same text is inside the window, which proves the test
        // measures the cap rather than a broken rule.
        assert!(m.evaluate(&lines[..MAX_REGION_LINES]).is_some());
    }

    #[test]
    fn every_builtin_manifest_parses() {
        let ids: Vec<&str> = builtin().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "codex", "generic"]);
    }

    #[test]
    fn for_program_is_case_insensitive_and_unknown_programs_have_no_manifest() {
        assert_eq!(for_program("CLAUDE").map(|m| m.id.as_str()), Some("claude"));
        assert_eq!(
            for_program("generic").map(|m| m.id.as_str()),
            Some("generic")
        );
        assert!(for_program("vim").is_none());
    }

    fn verdict(program: &str, lines: &[&str]) -> Option<Verdict> {
        for_program(program)
            .expect("the manifest is compiled in")
            .evaluate(&screen(lines))
    }

    #[test]
    fn the_claude_manifest_reads_the_esc_to_interrupt_spinner_as_working() {
        let v = verdict("claude", &["⏵ Cogitating… (12s · esc to interrupt)", ""])
            .expect("the spinner line is working");
        assert_eq!(v.state, AgentRuntimeState::Working);
        assert!(v.visible_working);
    }

    #[test]
    fn the_claude_manifest_reads_a_numbered_permission_prompt_as_a_visible_blocker() {
        let v = verdict(
            "claude",
            &[
                "Bash(rm -rf build)",
                "Do you want to proceed?",
                "❯ 1. Yes",
                "  2. Yes, and don't ask again",
                "  3. No, and tell Claude what to do differently",
            ],
        )
        .expect("a permission prompt is a blocker");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
        assert!(v.visible_blocker, "ADR-008 lets this override a hook");
    }

    #[test]
    fn the_claude_manifest_still_sees_the_prompt_when_the_cursor_has_moved_off_the_first_option() {
        // The selected option carries the marker, so a pattern that requires it on option
        // one misses every render where the person arrowed down.
        let v = verdict(
            "claude",
            &[
                "Do you want to proceed?",
                "  1. Yes",
                "❯ 2. No, and tell Claude what to do differently",
            ],
        )
        .expect("the resting render is the same prompt");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
    }

    #[test]
    fn the_claude_manifest_reports_nothing_while_the_transcript_viewer_is_open() {
        assert_eq!(
            verdict(
                "claude",
                &[
                    "Do you want to proceed?",
                    "  1. Yes",
                    "Showing detailed transcript · ctrl+o to toggle",
                ],
            ),
            None
        );
    }

    #[test]
    fn the_claude_manifest_reads_an_empty_composer_as_idle() {
        let v = verdict(
            "claude",
            &[
                "╭────────────────────────────╮",
                "│ >                          │",
                "╰────────────────────────────╯",
                "  ? for shortcuts",
            ],
        )
        .expect("an empty composer is an idle prompt");
        assert_eq!(v.state, AgentRuntimeState::Idle);
        assert!(v.visible_idle);
    }

    #[test]
    fn the_codex_manifest_reads_its_working_line_as_working() {
        let v =
            verdict("codex", &["• Working (2m 14s · esc to interrupt)"]).expect("codex is working");
        assert_eq!(v.state, AgentRuntimeState::Working);
        assert!(v.visible_working);
    }

    #[test]
    fn the_codex_manifest_reads_an_approval_request_as_a_visible_blocker() {
        let v = verdict("codex", &["Allow command?", "  Yes  No"]).expect("an approval request");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
        assert!(v.visible_blocker);
    }

    #[test]
    fn the_codex_manifest_reads_the_trust_dialog_as_blocked() {
        let v = verdict(
            "codex",
            &[
                "> You are in ~/coding/qoro",
                "  Do you trust the contents of this directory?",
            ],
        )
        .expect("the trust dialog blocks startup");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
    }

    #[test]
    fn the_codex_manifest_ignores_an_approval_that_scrolled_above_the_live_prompt() {
        // The same text below a shell prompt is history. This is the region doing the work
        // that a bare regex over the screen cannot do.
        assert_eq!(
            verdict(
                "codex",
                &["Allow command?", "  Yes  No", "user@host ~/coding/qoro $ "],
            ),
            None
        );
    }

    #[test]
    fn the_generic_manifest_reads_a_bare_shell_prompt_as_idle() {
        let v = verdict("generic", &["user@host ~/coding/qoro %"]).expect("a bare prompt");
        assert_eq!(v.state, AgentRuntimeState::Idle);
        assert!(v.visible_idle);
    }

    #[test]
    fn the_generic_manifest_claims_nothing_while_a_command_is_running() {
        assert_eq!(verdict("generic", &["$ cargo test", "   Compiling"]), None);
    }
}

#[cfg(test)]
mod against_a_real_screen {
    use super::*;

    #[test]
    fn the_screen_a_stand_in_agent_draws_reads_as_blocked() {
        // The exact rows a terminal holds after the script in `interface.rs` has run: the
        // question, the two choices, and the prompt the shell drew before it.
        let screen: Vec<String> = [
            "sh-3.2$ /tmp/bin/claude",
            "Do you want to proceed with this edit?",
            "  1. Yes",
            "  2. No",
            "",
            "",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let manifest = for_program("claude").expect("the bundled claude rules");
        let verdict = manifest.evaluate(&screen);
        assert_eq!(
            verdict.as_ref().map(|v| v.state),
            Some(qoro_domain::AgentRuntimeState::Blocked),
            "matched {:?}",
            verdict.as_ref().map(|v| &v.rule)
        );
    }
}

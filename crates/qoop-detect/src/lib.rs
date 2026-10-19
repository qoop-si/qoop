//! What the screen says an agent is doing.
//!
//! `qoop` knows which program runs in a terminal from the process table, and that is a fact.
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
use std::sync::{OnceLock, RwLock};

use regex::Regex;
use serde::Deserialize;

use qoop_domain::AgentRuntimeState;

pub use region::OscContext;
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
    resume: Option<Resume>,
}

/// The two command lines that carry a conversation across a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    /// Carry on one exact conversation. `{session}` stands for the agent's own id for it.
    /// Empty when the agent has no way to name a conversation from the command line.
    pub session: Vec<String>,
    /// Carry on the newest conversation in the directory, for when no id was ever learnt.
    /// Empty when the agent cannot do that either, and then a restored tab is just a shell.
    pub latest: Vec<String>,
}

/// What `{session}` is replaced with.
const SESSION_PLACEHOLDER: &str = "{session}";

impl Resume {
    /// The command line to run, given what is known about the conversation.
    ///
    /// An id is used when the agent can take one and one was learnt. Otherwise the newest
    /// conversation in the directory, which is right far more often than it sounds: a tab
    /// is opened in a directory and the agent in it is the one that was working there.
    pub fn argv(&self, session: Option<&str>) -> Option<Vec<String>> {
        match session {
            Some(id) if !self.session.is_empty() => Some(
                self.session
                    .iter()
                    .map(|arg| arg.replace(SESSION_PLACEHOLDER, id))
                    .collect(),
            ),
            _ if !self.latest.is_empty() => Some(self.latest.clone()),
            _ => None,
        }
    }
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
    /// Work in progress is positively on screen. A `working` rule without it only keeps a
    /// turn going; it cannot start one from idle.
    pub visible_working: bool,
    /// The matched rule is a startup gate: answering it is what creates the
    /// session rather than what continues one. A trust dialog on first run,
    /// not a permission prompt mid task. The runtime turns this into
    /// `trust_required` readiness, which is what an automation must not type
    /// over: bytes sent to a trust screen become a shell command, not a task.
    pub setup_gate: bool,
}

/// The whole answer to one look at a screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A rule matched and says what the agent is doing.
    Verdict(Verdict),
    /// A `skip_state_update` rule won: what is on screen is a view of the past, and the
    /// last reading should be left as it is.
    Skip { rule: String },
    /// No rule matched.
    Nothing,
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
    /// A manifest that needs a newer engine than this one.
    EngineTooNew { id: String, min_engine_version: u32 },
    /// A rule declares a state that is not an `AgentRuntimeState`.
    UnknownState { rule: String, state: String },
    /// A rule has no leaf test and no gate, so it would match every screen. Almost always a
    /// misspelled key, and at a high priority it would mask every rule below it.
    EmptyRule { rule: String },
    /// A resume command line that cannot do what its key says. Caught at parse time
    /// because the alternative is finding out at the one moment it matters, which is a
    /// person reopening their work and getting somebody else's conversation.
    BadResume { id: String, reason: String },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(message) => write!(f, "manifest is not valid: {message}"),
            Self::BadResume { id, reason } => {
                write!(f, "manifest {id}: the [resume] table {reason}")
            }
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
            Self::EngineTooNew {
                id,
                min_engine_version,
            } => write!(
                f,
                "manifest {id}: needs engine version {min_engine_version}, this is engine {ENGINE_VERSION}"
            ),
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
    /// A startup gate: the session does not exist until this is answered.
    /// Set in the manifest, never inferred, because only the rule's author
    /// knows whether the dialog creates a session or continues one.
    setup_gate: bool,
    /// The words of the option that trusts the folder, on a folder trust dialog: what the
    /// runtime picks for the person (see [`accept_keys`]).
    accept: Option<String>,
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
    /// Whether evaluating this gate can read the lowercased region copy.
    ///
    /// Only `contains` leaves touch `RegionView::lower`; every other leaf
    /// reads `text` or `lines`. The region materialises the lowercase copy
    /// only when this is true, so regex-only rules never pay for a second
    /// string they never read.
    fn needs_lower(&self) -> bool {
        self.contains.is_some()
            || self.all.iter().any(Gate::needs_lower)
            || self.any.iter().any(Gate::needs_lower)
            || self.not.iter().any(Gate::needs_lower)
    }

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
    /// The oldest engine that understands this file. Older engines refuse it
    /// outright rather than misreading rules written for newer semantics.
    #[serde(default)]
    min_engine_version: Option<u32>,
    #[serde(default)]
    rules: Vec<RawRule>,
    #[serde(default)]
    resume: Option<RawResume>,
}

/// How to put a person back into the conversation this agent was having.
///
/// Every line is the agent's own command line, spelled out in full, because the flags
/// differ per agent and change between releases: `claude --resume <id>` against `codex
/// resume <id>` against `opencode --session <id>`. A person whose agent changed its flag,
/// or who runs it through a wrapper of their own, edits this file rather than waiting for
/// a release.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResume {
    #[serde(default)]
    session: Vec<String>,
    #[serde(default)]
    latest: Vec<String>,
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
    /// A startup gate: answering creates the session rather than continuing
    /// one. False for every mid-task approval, which waits on a person but
    /// with a live session behind it.
    #[serde(default)]
    setup_gate: bool,
    #[serde(default)]
    accept: Option<String>,
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

/// Check a resume table says what it claims to say.
///
/// Both mistakes here are silent and expensive. A `session` line with no `{session}` in it
/// runs the same command for every conversation, so every restored tab lands in whichever
/// one the agent picks. A `latest` line with a `{session}` in it would carry the
/// placeholder through as a literal argument.
fn compile_resume(id: &str, raw: RawResume) -> Result<Resume, ManifestError> {
    let bad = |reason: &str| ManifestError::BadResume {
        id: id.to_string(),
        reason: reason.to_string(),
    };
    let mentions = |argv: &[String]| argv.iter().any(|a| a.contains(SESSION_PLACEHOLDER));
    if !raw.session.is_empty() && !mentions(&raw.session) {
        return Err(bad(
            "has a session line that never mentions {session}, so it would resume whichever \
             conversation the agent picks rather than the one this tab was having",
        ));
    }
    if mentions(&raw.latest) {
        return Err(bad(
            "has a latest line mentioning {session}, which is the line used when no session \
             id is known; the placeholder would be passed through as a literal argument",
        ));
    }
    for argv in [&raw.session, &raw.latest] {
        if argv.iter().any(|a| a.trim().is_empty()) {
            return Err(bad("has an empty argument, which no agent takes"));
        }
    }
    Ok(Resume {
        session: raw.session,
        latest: raw.latest,
    })
}

/// The command line that carries `program`'s conversation across a restart.
///
/// A user override wins over the bundled table, which is the whole point of the table
/// being in the manifest: an agent that changes its flag, or one run through a wrapper,
/// is a file the person edits rather than a release they wait for.
pub fn resume_argv(program: &str, session: Option<&str>) -> Option<Vec<String>> {
    for_program(program)?.resume()?.argv(session)
}

/// Whether a program can be resumed into a named conversation, not only the latest one.
///
/// What a sleep needs: coming back to "whichever conversation in this directory is newest"
/// is a different conversation the moment there are two.
pub fn resumes_by_id(program: &str) -> bool {
    for_program(program).is_some_and(|manifest| {
        manifest
            .resume()
            .is_some_and(|resume| !resume.session.is_empty())
    })
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
        if raw.min_engine_version.unwrap_or(0) > ENGINE_VERSION {
            return Err(ManifestError::EngineTooNew {
                id: raw.id,
                min_engine_version: raw.min_engine_version.unwrap_or(0),
            });
        }
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
                setup_gate: r.setup_gate,
                accept: r.accept,
                gate,
            });
        }
        let resume = raw.resume.map(|r| compile_resume(&raw.id, r)).transpose()?;
        Ok(Self {
            id: raw.id,
            version: raw.version,
            rules,
            resume,
        })
    }

    /// How to carry this agent's conversation across a restart, if it can be.
    pub fn resume(&self) -> Option<&Resume> {
        self.resume.as_ref()
    }

    /// The highest priority rule that matches, or `None` when the screen says nothing.
    ///
    /// A tie keeps the earliest declared rule, so a manifest's reading order is also its
    /// tie break and a person editing one can predict the outcome. `None` also comes back
    /// when the winning rule sets `skip_state_update`: see that field in the manifests.
    pub fn evaluate(&self, screen: &[String]) -> Option<Verdict> {
        self.evaluate_with_osc(screen, &OscContext { title: "" })
    }

    /// The same, with the OSC side channel attached.
    ///
    /// The split exists so screen-only callers keep working unchanged: with an
    /// empty title this behaves exactly like [`Manifest::evaluate`].
    pub fn evaluate_with_osc(&self, screen: &[String], osc: &OscContext<'_>) -> Option<Verdict> {
        match self.read_with_osc(screen, osc) {
            Reading::Verdict(v) => Some(v),
            Reading::Skip { .. } | Reading::Nothing => None,
        }
    }

    /// What the screen says, keeping apart the two ways of saying nothing.
    ///
    /// [`Manifest::evaluate_with_osc`] answers `None` both when no rule matched and when a
    /// `skip_state_update` rule won, and those mean opposite things to a caller that falls
    /// back to idle on silence: no match is an agent with nothing on screen, a skip is a
    /// pager over the past that says the last reading still stands.
    /// The option that trusts the folder on the gate `rule` names, when it is a folder trust
    /// dialog the runtime answers for the person.
    pub fn accept_of(&self, rule: &str) -> Option<&str> {
        self.rules
            .iter()
            .find(|r| r.id == rule && r.setup_gate)
            .and_then(|r| r.accept.as_deref())
    }

    pub fn read_with_osc(&self, screen: &[String], osc: &OscContext<'_>) -> Reading {
        let capped = &screen[screen.len().saturating_sub(MAX_REGION_LINES)..];
        let mut best: Option<&Rule> = None;
        for rule in &self.rules {
            // Strictly greater, so the first rule declared at a given priority holds it.
            // This also skips the matching work for rules that could not win anyway.
            if best.is_some_and(|b| b.priority >= rule.priority) {
                continue;
            }
            if rule
                .gate
                .matches(&rule.region.view(capped, osc, rule.gate.needs_lower()))
            {
                best = Some(rule);
            }
        }
        let Some(rule) = best else {
            return Reading::Nothing;
        };
        if rule.skip_state_update {
            return Reading::Skip {
                rule: rule.id.clone(),
            };
        }
        Reading::Verdict(Verdict {
            state: rule.state,
            rule: rule.id.clone(),
            visible_blocker: rule.visible_blocker,
            visible_idle: rule.visible_idle,
            visible_working: rule.visible_working,
            setup_gate: rule.setup_gate,
        })
    }
}

/// The next key toward the option reading `accept` in the menu on `screen`: the arrows from
/// the row the menu's pointer (`❯` or `›`) is on to that option's row, one per option (blank
/// rows between are not options), or enter once the pointer is on it. One step at a time, so
/// enter is pressed only on a screen that shows the pointer on the right option. Arrows are
/// spelled the way the program asked for (`application_cursor`: `SS3`, else `CSI`). `None`
/// when the screen shows no such option or no pointer near it, so nothing is typed at a screen
/// that is not the menu it was read as.
pub fn accept_keys(screen: &[String], accept: &str, application_cursor: bool) -> Option<Vec<u8>> {
    let target = screen.iter().rposition(|l| l.contains(accept))?;
    let pointed = |l: &str| {
        let l = l.trim_start();
        ['\u{276f}', '\u{203a}']
            .iter()
            .any(|p| l.strip_prefix(*p).is_some_and(|rest| rest.starts_with(' ')))
    };
    let from = target.saturating_sub(6);
    let cursor = screen[from..(target + 7).min(screen.len())]
        .iter()
        .position(|l| pointed(l))?
        + from;
    let between = screen[cursor.min(target)..cursor.max(target)]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .count();
    let arrow: &[u8] = match (target >= cursor, application_cursor) {
        (true, false) => b"\x1b[B",
        (true, true) => b"\x1bOB",
        (false, false) => b"\x1b[A",
        (false, true) => b"\x1bOA",
    };
    Some(if between == 0 {
        b"\r".to_vec()
    } else {
        arrow.repeat(between)
    })
}

/// The engine's own manifest version.
///
/// A manifest carrying a larger `min_engine_version` is refused at parse time
/// rather than misread: rules written for newer semantics failing open as
/// wrong states would be worse than no rules at all.
///
/// 2 added the `whole_recent`, `prompt_box_body`, `last_non_empty_above_prompt_box` and
/// `after_last_horizontal_rule` regions.
pub const ENGINE_VERSION: u32 = 2;

/// The manifests compiled into the binary, as source text.
const SOURCES: &[&str] = &[
    include_str!("../manifests/claude.toml"),
    include_str!("../manifests/codex.toml"),
    include_str!("../manifests/opencode.toml"),
    include_str!("../manifests/cursor.toml"),
    include_str!("../manifests/gemini.toml"),
    include_str!("../manifests/amp.toml"),
    // Manifests that say only how to carry a conversation across a restart. No rules yet,
    // for lack of a verified capture of each screen, so these agents are read by the
    // generic rules and resumed by their own.
    include_str!("../manifests/aider.toml"),
    include_str!("../manifests/goose.toml"),
    // Rules written from each agent's own source code and documentation, named in each
    // file's header.
    include_str!("../manifests/droid.toml"),
    include_str!("../manifests/qwen.toml"),
    include_str!("../manifests/grok.toml"),
    include_str!("../manifests/copilot.toml"),
    include_str!("../manifests/devin.toml"),
    include_str!("../manifests/kiro.toml"),
    include_str!("../manifests/kimi.toml"),
    include_str!("../manifests/kilo.toml"),
    include_str!("../manifests/hermes.toml"),
    include_str!("../manifests/cline.toml"),
    include_str!("../manifests/antigravity.toml"),
    include_str!("../manifests/muse.toml"),
    include_str!("../manifests/pi.toml"),
    include_str!("../manifests/maki.toml"),
    include_str!("../manifests/qodercli.toml"),
    include_str!("../manifests/generic.toml"),
];

static BUILTIN: OnceLock<Vec<Manifest>> = OnceLock::new();

/// The bundled manifests, keyed by lowercase id.
///
/// Parsed once from the same compiled-in sources as [`builtin`]: the parse
/// cannot fail in a shipped binary, and `every_builtin_manifest_parses` fails
/// the build if one of them is broken.
static BUNDLED: std::sync::LazyLock<std::collections::HashMap<String, std::sync::Arc<Manifest>>> =
    std::sync::LazyLock::new(|| {
        SOURCES
            .iter()
            .map(|src| {
                let manifest = Manifest::parse(src).expect("a bundled manifest must parse");
                (manifest.id.to_lowercase(), std::sync::Arc::new(manifest))
            })
            .collect()
    });

/// User overrides, keyed by lowercase manifest id. A daemon-side reload
/// replaces entries one file at a time: a bad file keeps the previous rules
/// for its agent and is reported, never loaded half way.
static OVERRIDES: std::sync::LazyLock<
    RwLock<std::collections::HashMap<String, std::sync::Arc<Manifest>>>,
> = std::sync::LazyLock::new(|| RwLock::new(std::collections::HashMap::new()));

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
/// A user override wins over the bundled manifest with the same id. The
/// comparison is case insensitive because the name arrives from a process
/// table. There is no fuzzy matching: an unrecognised program gets `None`,
/// and the caller decides whether to fall back to the `generic` manifest.
pub fn for_program(name: &str) -> Option<std::sync::Arc<Manifest>> {
    let key = name.to_lowercase();
    if let Ok(overrides) = OVERRIDES.read()
        && let Some(hit) = overrides.get(&key)
    {
        return Some(hit.clone());
    }
    BUNDLED.get(&key).cloned()
}

/// The rules to use for a program, falling back to the generic ones.
///
/// The fallback is the whole reason a terminal running a plain shell says anything at all.
/// Without it the caller asks for a manifest named `bash`, is told there is none, and the
/// state column stays empty for every terminal that is not one of the agents this
/// build happens to know: measured, exactly that, on a shell sitting at its own prompt.
pub fn rules_for(name: &str) -> Option<std::sync::Arc<Manifest>> {
    // A manifest with no rules in it is not a claim about the screen. Some agents have a
    // manifest only to say how to carry their conversation across a restart, and one of
    // those shadowing the generic rules would leave that agent read by nothing at all.
    for_program(name)
        .filter(|m| !m.rules.is_empty())
        .or_else(|| for_program("generic"))
}

/// What a reload of the override directory did, per file.
///
/// A file that fails to parse keeps the previous rules for its agent: the
/// table is never half loaded, and the error names the file and the reason.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Manifest ids now serving override rules, in filename order.
    pub loaded: Vec<String>,
    /// `(file stem, reason)` for every file that was skipped.
    pub errors: Vec<(String, String)>,
}

/// Re-read user override manifests from a directory.
///
/// Each `<agent>.toml` must declare `id = "<agent>"` matching its filename:
/// a mismatch is an error, not a silent rename. Files that fail to read or
/// parse are reported and leave the previous rules in place; files that parse
/// replace the entry for their id, including ids no bundled manifest has.
/// A missing directory is an empty success, not an error.
///
/// An override whose file is gone is dropped, and a directory that is gone drops
/// them all: deleting the file is how a person takes an override back, and a
/// reload that only ever inserted kept the rules until the daemon restarted.
pub fn reload_overrides(dir: &std::path::Path) -> ReloadOutcome {
    let mut outcome = ReloadOutcome::default();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    let Ok(mut overrides) = OVERRIDES.write() else {
        return outcome;
    };
    overrides.retain(|id, _| {
        files.iter().any(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| stem.eq_ignore_ascii_case(id))
        })
    });
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let text = match std::fs::read_to_string(&path) {
            Err(e) => {
                outcome.errors.push((stem, format!("could not read: {e}")));
                continue;
            }
            Ok(text) => text,
        };
        match Manifest::parse(&text) {
            Err(e) => outcome.errors.push((stem, e.to_string())),
            Ok(manifest) if manifest.id != stem => outcome.errors.push((
                stem.clone(),
                format!("declares id {:?}, expected {stem:?}", manifest.id),
            )),
            Ok(manifest) => {
                let id = manifest.id.clone();
                overrides.insert(id.to_lowercase(), std::sync::Arc::new(manifest));
                outcome.loaded.push(id);
            }
        }
    }
    outcome
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
    fn needs_lower_is_true_only_when_a_contains_leaf_exists() {
        // `contains` is the only leaf that reads the lowercased copy. This
        // proves the allocation is skipped, not just that results match:
        // a regex-only gate must report false here.
        let regex_only = Gate {
            regex: Some(Regex::new("proceed").expect("test regex")),
            ..Gate::default()
        };
        assert!(!regex_only.needs_lower());
        let line_regex_only = Gate {
            line_regex: Some(Regex::new("proceed").expect("test regex")),
            ..Gate::default()
        };
        assert!(!line_regex_only.needs_lower());
        let nested_no_contains = Gate {
            any: vec![regex_only],
            not: vec![line_regex_only],
            ..Gate::default()
        };
        assert!(!nested_no_contains.needs_lower());
        let nested_contains = Gate {
            any: vec![
                Gate {
                    regex: Some(Regex::new("x").expect("test regex")),
                    ..Gate::default()
                },
                Gate {
                    not: vec![Gate {
                        contains: Some("transcript".into()),
                        ..Gate::default()
                    }],
                    ..Gate::default()
                },
            ],
            ..Gate::default()
        };
        assert!(nested_contains.needs_lower());
    }

    #[test]
    fn contains_matches_whatever_case_the_screen_uses() {
        let m = one_rule("contains = \"Do You Want To Proceed\"\n");
        assert!(matched(&m, &["do you want to proceed?"]));
        assert!(matched(&m, &["DO YOU WANT TO PROCEED?"]));
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
    fn a_winning_skip_rule_is_read_as_a_skip_and_not_as_nothing() {
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
id = "working"
state = "working"
priority = 900
contains = "esc to interrupt"
"#,
        )
        .expect("parses");
        let osc = OscContext { title: "" };
        assert_eq!(
            m.read_with_osc(
                &screen(&["esc to interrupt", "showing detailed transcript"]),
                &osc
            ),
            Reading::Skip {
                rule: "transcript".into()
            }
        );
        assert_eq!(
            m.read_with_osc(&screen(&["nothing here"]), &osc),
            Reading::Nothing
        );
        assert!(matches!(
            m.read_with_osc(&screen(&["esc to interrupt"]), &osc),
            Reading::Verdict(v) if v.rule == "working"
        ));
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
        let err = one_rule_err("region = \"above_prompt_box\"\ncontains = \"x\"\n");
        assert_eq!(
            err,
            ManifestError::UnknownRegion {
                rule: "r".to_string(),
                region: "above_prompt_box".to_string(),
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
        assert_eq!(
            ids,
            vec![
                "claude",
                "codex",
                "opencode",
                "cursor",
                "gemini",
                "amp",
                "aider",
                "goose",
                "droid",
                "qwen",
                "grok",
                "copilot",
                "devin",
                "kiro",
                "kimi",
                "kilo",
                "hermes",
                "cline",
                "antigravity",
                "muse",
                "pi",
                "maki",
                "qodercli",
                "generic"
            ]
        );
    }

    #[test]
    fn for_program_is_case_insensitive_and_unknown_programs_have_no_manifest() {
        assert_eq!(
            for_program("CLAUDE").map(|m| m.id.clone()),
            Some("claude".to_string())
        );
        assert_eq!(
            for_program("generic").map(|m| m.id.clone()),
            Some("generic".to_string())
        );
        assert!(for_program("vim").is_none());
    }

    /// A verdict from the manifests as they stand, taken while no override test is
    /// swapping them out.
    ///
    /// The override table is one process global and cargo runs these tests in parallel
    /// threads, so without this an override installed by one test answered another test's
    /// question: measured, roughly one run in three failed on whichever claude test
    /// happened to overlap. Holding it here rather than in each test keeps the guarantee
    /// where the shared state is read.
    fn verdict(program: &str, lines: &[&str]) -> Option<Verdict> {
        let _held = OVERRIDE_TEST_LOCK.read().unwrap();
        verdict_unlocked(program, lines)
    }

    /// The same, for a caller that already holds the lock.
    fn verdict_unlocked(program: &str, lines: &[&str]) -> Option<Verdict> {
        for_program(program)
            .expect("the manifest is compiled in")
            .evaluate(&screen(lines))
    }

    fn verdict_with_title(program: &str, lines: &[&str], title: &str) -> Option<Verdict> {
        let _held = OVERRIDE_TEST_LOCK.read().unwrap();
        for_program(program)
            .expect("the manifest is compiled in")
            .evaluate_with_osc(&screen(lines), &OscContext { title })
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

    /// A question menu is a person being asked to choose, even when the options
    /// are not yes or no. Copied off a real session where the model asked what
    /// to do next and the panel kept saying working.
    #[test]
    fn the_claude_manifest_reads_a_question_menu_as_a_visible_blocker() {
        let v = verdict(
            "claude",
            &[
                "Elde ~1700 satir commit edilmemis isi var. Bununla ne yapalim?",
                "  1. Once durumu oku ve anlat",
                "  2. Kontrolleri calistir",
                "❯ 3. Palette isini bitir",
                "  4. Bu is degil, baska konu",
                "Enter to select · ↑/↓ to navigate · Esc to cancel",
            ],
        )
        .expect("a question menu is a blocker");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
        assert!(v.visible_blocker);
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

    /// The line a real Codex draws for the first half minute of a session, copied off the
    /// screen. It is Codex holding the turn and it does not contain the word "Working",
    /// which is what the rule used to be anchored on.
    #[test]
    fn the_codex_manifest_reads_any_interrupt_hint_as_working() {
        let v = verdict(
            "codex",
            &["• Starting MCP servers (6/7): yargi-mcp (14s · esc to interrupt)"],
        )
        .expect("codex is working");
        assert_eq!(v.state, AgentRuntimeState::Working);
        assert!(v.visible_working);
    }

    /// The composer, which is on screen only while Codex is waiting for a person, copied
    /// off a real session that had just finished starting up.
    #[test]
    fn the_codex_manifest_reads_its_composer_as_idle() {
        for composer in ["› Ask Codex to do anything", "› write me a test"] {
            let v = verdict("codex", &[composer, "  gpt-6-astra medium · ~/coding/qoop"])
                .unwrap_or_else(|| panic!("codex is idle: {composer}"));
            assert_eq!(v.state, AgentRuntimeState::Idle, "{composer}");
            assert!(v.visible_idle);
        }
    }

    /// The two cannot both be on screen, and when the transcript happens to carry a line
    /// that looks like a composer, the status line has to win.
    #[test]
    fn a_codex_that_is_working_is_not_also_idle() {
        let v = verdict(
            "codex",
            &[
                "› an earlier prompt still in the transcript",
                "• Working (2m 14s · esc to interrupt)",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Working);
    }

    /// Copied off a real session: Codex draws its update notice as a numbered menu, over
    /// the row the composer normally has and with the same marker on it. Read as a composer
    /// it reports idle, which is a person being asked a question and told nobody is waiting.
    #[test]
    fn a_numbered_menu_is_a_person_being_asked_and_not_an_idle_composer() {
        let v = verdict(
            "codex",
            &[
                "  \u{2728} Update available! 0.153.4 -> 0.154.0",
                "\u{203A} 1. Update now (runs `sh -c 'curl -fsSL https://example.com/i.sh | sh'`)",
                "  2. Skip",
                "  3. Skip until next version",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Blocked);
        assert!(v.visible_blocker);
    }

    /// One numbered option carrying the composer's own marker: too few lines for the menu
    /// rule to fire, so the composer rule answers alone and its exclusion is the only thing
    /// standing between a question and the word `idle`.
    #[test]
    fn the_composer_rule_alone_does_not_read_a_numbered_option_as_a_prompt() {
        let v = verdict("codex", &["\u{203A} 1. Update now"]);
        assert!(
            !v.as_ref()
                .is_some_and(|v| v.state == AgentRuntimeState::Idle),
            "a numbered option read as an idle composer: {v:?}"
        );
    }

    /// A finished answer that ends in a numbered summary, with the composer under it. The
    /// list is two numbered lines inside the menu rule's window, and without the marker a
    /// numbered option carries it reads as blocked: the row lights up and the bell rings for
    /// an agent that is done and waiting to be typed to.
    #[test]
    fn a_finished_answer_ending_in_a_numbered_list_is_idle() {
        let v = verdict(
            "codex",
            &[
                "\u{2022} Done. Two changes:",
                "  1. the parser keeps the trailing byte",
                "  2. a test covers the split read",
                "\u{203A} Ask Codex to do anything",
                "  gpt-6-astra medium \u{b7} ~/coding/qoop",
            ],
        )
        .expect("codex is idle");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
    }

    /// One numbered line is ordinary model output. Two is somebody being asked to choose.
    /// Without the second, a running agent reads as waiting, and `visible_blocker` makes
    /// that reading outrank everything weaker under ADR-008.
    #[test]
    fn one_numbered_line_in_ordinary_output_is_not_a_menu() {
        let v = verdict(
            "codex",
            &[
                "\u{2022} Thinking",
                "1. first do this",
                "then carry on with the rest of it",
            ],
        );
        assert!(
            !v.as_ref()
                .is_some_and(|v| v.state == AgentRuntimeState::Blocked),
            "a single numbered line read as a question: {v:?}"
        );
    }

    /// Copied off a real session, bottom rows and all: Codex draws its status line and then
    /// keeps drawing under it, so the line that says the turn is happening is the fourth
    /// non empty row from the bottom and not the first.
    #[test]
    fn the_codex_working_line_is_seen_under_its_rule_composer_and_footer() {
        let v = verdict(
            "codex",
            &[
                "  59",
                "  60",
                "\u{2022} Working (11s \u{b7} esc to interrupt)",
                "",
                &"\u{2500}".repeat(60),
                "\u{203A} Ask Codex to do anything",
                "",
                "  gpt-5.6-sol high \u{b7} ~/coding/qoop \u{b7} Count 1 to 40 slowly",
            ],
        )
        .expect("codex is working");
        assert_eq!(v.state, AgentRuntimeState::Working, "{v:?}");
    }

    #[test]
    fn the_codex_manifest_ignores_an_approval_that_scrolled_above_the_live_prompt() {
        // The same text below a shell prompt is history. This is the region doing the work
        // that a bare regex over the screen cannot do.
        assert_eq!(
            verdict(
                "codex",
                &["Allow command?", "  Yes  No", "user@host ~/coding/qoop $ "],
            ),
            None
        );
    }

    #[test]
    fn the_generic_manifest_reads_a_bare_shell_prompt_as_idle() {
        let v = verdict("generic", &["user@host ~/coding/qoop %"]).expect("a bare prompt");
        assert_eq!(v.state, AgentRuntimeState::Idle);
        assert!(v.visible_idle);
    }

    #[test]
    fn the_generic_manifest_claims_nothing_while_a_command_is_running() {
        assert_eq!(verdict("generic", &["$ cargo test", "   Compiling"]), None);
    }

    /// opencode's permission dialog at each of its stages, laid out as its own source
    /// draws it (packages/tui/src/routes/session/permission.tsx at v1.18.32, the installed
    /// version): the dialog takes the composer's place at the bottom, and its key hints
    /// sit one border-only row above the session's blank last row.
    const OPENCODE_TRANSCRIPT: &[&str] = &[
        "     I'll clear the old build output first.",
        "",
        "     # Shell command",
        "",
    ];

    fn opencode_screen(dialog: &[&'static str]) -> Vec<&'static str> {
        let mut lines = OPENCODE_TRANSCRIPT.to_vec();
        lines.extend_from_slice(dialog);
        lines.push("");
        lines
    }

    #[test]
    fn a_live_opencode_permission_reads_blocked_at_every_stage() {
        let inline = [
            "  ┃",
            "  ┃  △ Permission required",
            "  ┃    # Shell command",
            "  ┃",
            "  ┃  $ rm -rf build",
            "  ┃",
            "  ┃",
            "  ┃   Allow once   Allow always   Reject       ctrl+f fullscreen  ⇆ select  enter confirm",
            "  ┃",
        ];
        // At 80 to 88 columns the boxes do not shrink and the row is cut short.
        let cut = [
            "  ┃",
            "  ┃  △ Permission required",
            "  ┃",
            "  ┃   Allow once   Allow always   Reject       ctrl+f fullscreen  ⇆ select  enter co",
            "  ┃",
        ];
        let v = verdict("opencode", &cut).expect("a cut permission row is still a verdict");
        assert_eq!(v.state, AgentRuntimeState::Blocked, "{v:?}");
        // Under 80 columns the options and the hints stack.
        let narrow = [
            "  ┃",
            "  ┃  △ Permission required",
            "  ┃    # Shell command",
            "  ┃",
            "  ┃  $ rm -rf build",
            "  ┃",
            "  ┃",
            "  ┃   Allow once   Allow always   Reject",
            "  ┃",
            "  ┃  ctrl+f fullscreen  ⇆ select  enter confirm",
            "  ┃",
        ];
        // Fullscreen: the header goes to the top of the screen and a diff fills the rest,
        // so the header is far above any bottom region.
        let mut fullscreen = vec![
            "  ┃",
            "  ┃  △ Permission required",
            "  ┃    → Edit src/lib.rs",
            "  ┃",
        ];
        fullscreen.extend(std::iter::repeat_n(
            "  ┃   12   let total = parts.iter().sum::<u64>();",
            30,
        ));
        fullscreen.extend([
            "  ┃",
            "  ┃",
            "  ┃   Allow once   Allow always   Reject       ctrl+f minimize  ⇆ select  enter confirm",
            "  ┃",
        ]);
        // "Allow always" asks once more before it applies.
        let always = [
            "  ┃",
            "  ┃  △ Always allow",
            "  ┃",
            "  ┃  This will allow the following patterns until OpenCode is restarted",
            "  ┃",
            "  ┃  - rm -rf *",
            "  ┃",
            "  ┃",
            "  ┃   Confirm   Cancel                                     ⇆ select  enter confirm",
            "  ┃",
        ];
        // "Reject" in a subagent asks what to do instead.
        let reject = [
            "  ┃",
            "  ┃  △ Reject permission",
            "  ┃",
            "  ┃  Tell OpenCode what to do differently",
            "  ┃",
            "  ┃",
            "  ┃  use make clean instead                            enter confirm  esc cancel",
            "  ┃",
        ];
        for (stage, dialog) in [
            ("inline", &inline[..]),
            ("narrow", &narrow[..]),
            ("fullscreen", &fullscreen[..]),
            ("always", &always[..]),
            ("reject", &reject[..]),
        ] {
            let v = verdict("opencode", &opencode_screen(dialog));
            let v = v.unwrap_or_else(|| panic!("{stage}: a live permission read nothing"));
            assert_eq!(v.state, AgentRuntimeState::Blocked, "{stage}: {v:?}");
            assert!(v.visible_blocker, "{stage}");
        }
    }

    /// The folder trust dialog as Claude Code draws it now, captured from a
    /// real start in a new directory. Its default option exits.
    const CLAUDE_TRUST: &[&str] = &[
        "────────────────────────────────────────────────────────────────────────────────",
        " Accessing workspace:",
        " /private/tmp/drill",
        " Quick safety check: Is this a project you created or one you trust? (Like your",
        " own code, a well-known open source project, or work from your team). If not,",
        " take a moment to review what's in this folder first.",
        " Claude Code'll be able to read, edit, and execute files here.",
        " Security guide",
        " \u{276F} No, exit",
        "   Yes, I trust this folder",
        " Enter to confirm · Esc to cancel",
    ];

    #[test]
    fn the_claude_manifest_reads_the_trust_dialog_as_a_setup_gate() {
        let v = verdict("claude", CLAUDE_TRUST).expect("the trust dialog blocks startup");
        assert_eq!(v.state, AgentRuntimeState::Blocked, "{v:?}");
        assert_eq!(v.rule, "trust_folder");
        assert!(v.setup_gate, "answering it creates the session: {v:?}");
    }

    #[test]
    fn claude_quoting_the_trust_dialog_is_not_the_dialog() {
        // The manifest itself on screen, as `cat claude.toml` in a Claude session
        // draws it: every phrase is there, none of them as the dialog's own line.
        let manifest = include_str!("../manifests/claude.toml");
        let lines: Vec<&str> = manifest.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("id = \"trust_folder\""))
            .expect("the rule is in the manifest");
        let shown = &lines[at.saturating_sub(6)..(at + 12).min(lines.len())];
        let v = verdict("claude", shown);
        assert!(v.as_ref().is_none_or(|v| v.rule != "trust_folder"), "{v:?}");
    }

    #[test]
    fn an_interrupted_claude_turn_reads_as_a_visible_idle_composer() {
        // Captured after Esc: the turn is over and Claude has put the prompt
        // back in the composer. No Stop hook runs on an interrupt, so this
        // screen is the only thing that can say the agent is idle.
        let v = verdict(
            "claude",
            &[
                "\u{273B} Crunched for 1s · done 1:16 PM",
                "────────────────────────────────────────────────────────────────────────────────",
                "\u{276F} Write a 3000 word essay about the history of terminals. Do not use tools.",
                "────────────────────────────────────────────────────────────────────────────────",
                "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
        assert!(v.visible_idle);
    }

    /// Frames captured from a live Claude Code turn at 110 columns. `RULE` is the box's
    /// horizontal rule; the footer loses its interrupt hint whenever a draft is typed.
    const RULE: &str =
        "──────────────────────────────────────────────────────────────────────────────";
    const DRAFT_FOOTER: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";

    /// A draft typed while the answer streams: no spinner, no interrupt hint. 103 of 120
    /// frames looked like this and read as a visible idle, so the tab flickered.
    #[test]
    fn a_claude_draft_under_a_streaming_answer_is_not_visible_idle() {
        let v = verdict(
            "claude",
            &[
                "  were pressed in quick succession, they could jam. Sholes and his partners rearranged",
                "  collisions and let typists work faster without jams.",
                "                                              You've used 75% of your weekly limit",
                RULE,
                "\u{276F} taslak",
                RULE,
                DRAFT_FOOTER,
            ],
        )
        .expect("a verdict");
        assert!(!v.visible_idle, "{v:?}");
    }

    /// The prompt just sent sits in the transcript between the last turn's summary and the
    /// box, so the summary is the previous turn's and says nothing about this one.
    #[test]
    fn a_claude_summary_above_a_prompt_just_sent_is_not_the_end_of_the_turn() {
        let v = verdict(
            "claude",
            &[
                "\u{273B} Brewed for 32s \u{00B7} done 6:46 PM",
                "\u{276F} Write a 1500 word essay about pianos. No tools.",
                RULE,
                "\u{276F} taslak",
                RULE,
                DRAFT_FOOTER,
            ],
        )
        .expect("a verdict");
        assert!(!v.visible_idle, "{v:?}");
    }

    /// The turn's summary right above a draft, with a notice between: the turn is over.
    #[test]
    fn a_finished_claude_turn_is_visible_idle_under_a_draft() {
        let v = verdict(
            "claude",
            &[
                "\u{273B} Crunched for 16m 31s \u{00B7} done 6:31 PM",
                "                              You've used 75% of your weekly limit",
                RULE,
                "\u{276F} evet kapat hepsini",
                RULE,
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} 5 agents",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
        assert!(v.visible_idle);
    }

    /// An early Esc puts the prompt back in the box, and notices can stack above it.
    #[test]
    fn a_claude_interrupt_under_several_notices_is_visible_idle() {
        let v = verdict(
            "claude",
            &[
                "  \u{23BF}  Interrupted \u{00B7} What should Claude do instead?",
                "                    tmux focus-events off \u{00B7} add 'set -g focus-events on'",
                "                    You've used 75% of your weekly limit",
                "                    tmux detected \u{00B7} scroll with PgUp/PgDn",
                RULE,
                "\u{276F} Write a 1500 word essay about pianos. No tools.",
                RULE,
                DRAFT_FOOTER,
            ],
        )
        .expect("a verdict");
        assert!(v.visible_idle);
    }

    /// Esc in the middle of a streamed answer: the note, and an empty box.
    #[test]
    fn a_claude_turn_interrupted_mid_answer_is_visible_idle() {
        let v = verdict(
            "claude",
            &[
                "  violin's design through constant experimentation.",
                "  \u{23BF}  Interrupted \u{00B7} What should Claude do instead?",
                RULE,
                "\u{276F} ",
                RULE,
                "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle) \u{00B7} \u{2190} 5 agents",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
        assert!(v.visible_idle);
    }

    /// The same answer streaming with the box empty: the footer's interrupt hint holds it.
    #[test]
    fn a_claude_answer_streaming_over_an_empty_box_is_working() {
        let v = verdict(
            "claude",
            &[
                "  Stradivari worked in Cremona for most of his long life, from the 1660s until",
                RULE,
                "\u{276F} ",
                RULE,
                "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} 5 agents",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Working, "{v:?}");
    }

    #[test]
    fn a_claude_draft_does_not_hide_a_running_turn() {
        let v = verdict(
            "claude",
            &[
                "\u{273B} Cogitating… (12s · esc to interrupt)",
                "────────────────────────────────────────────────────────────────────────────────",
                "\u{276F} and then run the tests",
                "────────────────────────────────────────────────────────────────────────────────",
                "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)",
            ],
        )
        .expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Working, "{v:?}");
    }

    #[test]
    fn a_past_claude_prompt_in_the_transcript_is_not_a_composer() {
        // The transcript marks past prompts with the composer's own marker,
        // but outside the ruled box.
        let v = verdict(
            "claude",
            &[
                "\u{276F} Reply with exactly the text DRILL_OK_42",
                "",
                "\u{23FA} DRILL_OK_42",
            ],
        );
        assert!(
            v.as_ref().is_none_or(|v| v.rule != "live_prompt_box"),
            "{v:?}"
        );
    }

    #[test]
    fn a_claude_form_between_rules_is_not_a_draft() {
        let v = verdict(
            "claude",
            &[
                "────────────────────────────────────────────────────────────────────────────────",
                "\u{276F} Sonnet",
                "  Opus",
                "────────────────────────────────────────────────────────────────────────────────",
                " Enter to select · Esc to cancel",
            ],
        )
        .expect("a verdict");
        // A picker waiting on the person is never read as a draft.
        assert!(!v.visible_idle, "{v:?}");
    }

    /// Captured from a live turn with a draft in the composer: the interrupt hint is gone
    /// from the footer, so the spinner line is the only working evidence, and every glyph
    /// it cycles through has to count. This frame read as the idle prompt box.
    #[test]
    fn every_claude_spinner_frame_holds_working_over_a_draft() {
        for glyph in [
            '\u{00B7}', '\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}', '\u{273D}',
        ] {
            let spinner =
                format!("{glyph} Stewing\u{2026} (6s \u{00B7} thinking with high effort)");
            let v = verdict(
                "claude",
                &[
                    "\u{276F} Write a 2500 word essay about the history of terminals. No tools.",
                    &spinner,
                    "────────────────────────────────────────────────────────────────────────────────",
                    "\u{276F} taslak metin burada",
                    "────────────────────────────────────────────────────────────────────────────────",
                    "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)",
                ],
            )
            .expect("a verdict");
            assert_eq!(v.state, AgentRuntimeState::Working, "{glyph}: {v:?}");
        }
    }

    /// Claude keeps the same `\u{2733}` title while it works, so the title may say idle
    /// but never as visible evidence that outranks a hook.
    #[test]
    fn claudes_title_is_not_visible_idle() {
        let v = verdict_with_title("claude", &["some output"], "\u{2733} Terminal essay");
        assert!(v.as_ref().is_none_or(|v| !v.visible_idle), "{v:?}");
    }

    /// The composer as opencode v1.18.32 draws it (packages/tui/src/component/prompt/
    /// index.tsx) above the transcript lines given: the input, the agent and model row, the
    /// bottom edge, and the status row that tells a turn in progress from a finished one.
    fn opencode_composer(
        transcript: &[&'static str],
        input: &'static str,
        status: &'static str,
    ) -> Vec<&'static str> {
        let mut lines = transcript.to_vec();
        lines.extend([
            "",
            "  ┃",
            input,
            "  ┃",
            "  ┃  Build · Claude Sonnet 4.5 Anthropic",
            "  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            status,
            "",
        ]);
        lines
    }

    const OPENCODE_EMPTY_INPUT: &str = "  ┃  Ask anything… \"Fix a TODO in the codebase\"";
    const OPENCODE_IDLE_ROW: &str =
        "   ~/coding/site                                   12.4K (6%) · $0.04  ctrl+p commands";
    const OPENCODE_BUSY_ROW: &str =
        "   ⬝⬝■■■⬝⬝⬝ esc interrupt                          12.4K (6%) · $0.04  ctrl+p commands";
    const OPENCODE_BUSY_AGAIN_ROW: &str =
        "   ■■■⬝⬝⬝⬝⬝ esc again to interrupt                 12.4K (6%) · $0.04  ctrl+p commands";

    /// A narrow tile cuts the interrupt hint but not the spinner, whose box never shrinks,
    /// and the key hints can still be there: that is a turn in progress, not idle.
    #[test]
    fn a_busy_opencode_row_cut_short_is_still_working() {
        let screen = opencode_composer(
            &["     Working on it."],
            OPENCODE_EMPTY_INPUT,
            "   ⬝⬝■■■⬝⬝⬝ esc in  ctrl+p commands",
        );
        let v = verdict("opencode", &screen).expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Working, "{v:?}");
    }

    /// Once answered, the dialog is gone and the composer is back. Its words above the
    /// composer, left there or quoted in an answer, are the past and must not block.
    #[test]
    fn an_answered_opencode_permission_above_the_composer_is_not_blocked() {
        let answered = opencode_composer(
            &[
                "  ┃  △ Permission required",
                "  ┃    # Shell command",
                "  ┃  $ rm -rf build",
                "  ┃   Allow once   Allow always   Reject       ctrl+f fullscreen  ⇆ select  enter confirm",
                "  ┃",
            ],
            OPENCODE_EMPTY_INPUT,
            OPENCODE_IDLE_ROW,
        );
        let quoted = opencode_composer(
            &[
                "     The deploy stopped: permission required to write /var/www.",
                "     Run it again with sudo, or change the directory's owner.",
            ],
            OPENCODE_EMPTY_INPUT,
            OPENCODE_IDLE_ROW,
        );
        for (case, screen) in [("answered", answered), ("quoted", quoted)] {
            let v = verdict("opencode", &screen).expect("a composer is idle");
            assert_eq!(v.state, AgentRuntimeState::Idle, "{case}: {v:?}");
        }
    }

    #[test]
    fn the_opencode_manifest_reads_a_turn_in_progress_as_working() {
        let screen = opencode_composer(
            &["     Reading the build script first."],
            "  ┃  also check the tests",
            OPENCODE_BUSY_ROW,
        );
        let v = verdict("opencode", &screen).expect("a turn in progress");
        assert_eq!(v.state, AgentRuntimeState::Working, "{v:?}");
        assert!(v.visible_working);
    }

    #[test]
    fn the_opencode_manifest_reads_its_composer_as_idle() {
        let screen = opencode_composer(
            &["     Done: the build passes."],
            OPENCODE_EMPTY_INPUT,
            OPENCODE_IDLE_ROW,
        );
        let v = verdict("opencode", &screen).expect("a composer is idle");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
        assert!(v.visible_idle);
        // The home screen has no session, so its status row is the key hints alone.
        let home = opencode_composer(&[], OPENCODE_EMPTY_INPUT, "   tab agents  ctrl+p commands");
        let v = verdict("opencode", &home).expect("the home composer is idle");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
    }

    /// A real opencode 1.18.32 start, captured from a qoop terminal: the key hints sit
    /// above a tip and the directory, so the idle row is third from the bottom.
    #[test]
    fn a_real_opencode_home_screen_reads_idle() {
        let screen = [
            r#"                                                          ▄"#,
            r#"                         █▀▀█ █▀▀█ █▀▀█ █▀▀▄ █▀▀▀ █▀▀█ █▀▀█ █▀▀█"#,
            r#"                         █  █ █  █ █▀▀▀ █  █ █    █  █ █  █ █▀▀▀"#,
            r#"                         ▀▀▀▀ █▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀"#,
            r#""#,
            r#""#,
            r#"       ┃"#,
            r#"       ┃  Ask anything… "Fix broken tests""#,
            r#"       ┃"#,
            r#"       ┃  Build · Muse Spark 1.3 Free OpenCode Zen"#,
            r#"       ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀"#,
            r#"                                                       tab agents  ctrl+p commands"#,
            r#""#,
            r#""#,
            r#""#,
            r#"                ● Tip Run /connect to add an AI provider and start coding"#,
            r#""#,
            r#""#,
            r#""#,
            r#"  /private/tmp                                                                  1.18.32"#,
        ];
        let v = verdict("opencode", &screen).expect("the home screen is idle");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
    }

    #[test]
    fn an_opencode_interrupt_hint_in_the_transcript_does_not_hold_working() {
        // The words sit in the transcript while the finished agent's composer waits
        // below. Only the status row is read, so the agent reads idle, not working.
        let screen = opencode_composer(
            &[
                "     Press esc interrupt to stop a turn; ■■■■ is the spinner.",
                "     ▣  Build · Claude Sonnet 4.5 · 4.2s",
            ],
            OPENCODE_EMPTY_INPUT,
            OPENCODE_IDLE_ROW,
        );
        let v = verdict("opencode", &screen).expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
    }

    #[test]
    fn an_interrupted_opencode_turn_is_idle() {
        // Esc pressed twice: the turn ends, its footer says so, and the status row
        // drops the spinner and the hint.
        let screen = opencode_composer(
            &["     ▣  Build · Claude Sonnet 4.5 · 4.2s · interrupted"],
            OPENCODE_EMPTY_INPUT,
            OPENCODE_IDLE_ROW,
        );
        let v = verdict("opencode", &screen).expect("a verdict");
        assert_eq!(v.state, AgentRuntimeState::Idle, "{v:?}");
    }

    /// The turn the replay fixture plays, frame by frame: working, the first esc press,
    /// finished, a new turn, finished again.
    #[test]
    fn an_opencode_turn_reads_working_then_idle_then_working() {
        let transcript = ["     Reading the build script first."];
        for (row, want) in [
            (OPENCODE_BUSY_ROW, AgentRuntimeState::Working),
            (OPENCODE_BUSY_AGAIN_ROW, AgentRuntimeState::Working),
            (OPENCODE_IDLE_ROW, AgentRuntimeState::Idle),
            (OPENCODE_BUSY_ROW, AgentRuntimeState::Working),
            (OPENCODE_IDLE_ROW, AgentRuntimeState::Idle),
        ] {
            let screen = opencode_composer(&transcript, OPENCODE_EMPTY_INPUT, row);
            let v = verdict("opencode", &screen);
            assert_eq!(v.map(|v| v.state), Some(want), "{row}");
        }
    }

    #[test]
    fn without_a_title_the_osc_rules_are_silent() {
        // An empty title is no lines, so no `contains` leaf can match it, and
        // the screen-only entry point never sees a title at all.
        assert_eq!(verdict_with_title("amp", &[""], ""), None);
        assert_eq!(
            for_program("amp")
                .expect("the manifest is compiled in")
                .evaluate(&screen(&["Plugin confirmation needed on screen"])),
            None,
            "a title rule must not read screen text"
        );
    }

    #[test]
    fn agents_without_a_shipped_manifest_fall_back_to_generic() {
        // No agent gets rules without a capture of its screen behind them. These two
        // have neither and ship a manifest anyway, because how to carry a conversation
        // across a restart is a different question from how to read a screen and is
        // answered from an agent's own published flags. A manifest with no rules in it
        // must not shadow the generic ones.
        for name in ["aider", "goose"] {
            let manifest = for_program(name).unwrap_or_else(|| panic!("{name} has a manifest"));
            assert!(
                manifest.rules.is_empty(),
                "{name} has rules that no capture justifies"
            );
            assert!(
                manifest.resume().is_some(),
                "{name} has a manifest that says nothing at all"
            );
            assert_eq!(
                rules_for(name).map(|m| m.id.clone()),
                Some("generic".to_string()),
                "{name} is read by its own empty rules rather than the generic ones"
            );
        }
        // And a program with no manifest at all still falls back.
        assert!(for_program("crush").is_none());
        assert_eq!(
            rules_for("crush").map(|m| m.id.clone()),
            Some("generic".to_string())
        );
    }

    #[test]
    fn a_manifest_needing_a_newer_engine_is_refused_naming_itself() {
        let err = Manifest::parse(
            "id = \"future\"\nversion = \"9\"\nmin_engine_version = 999\n\n[[rules]]\nid = \"r\"\nstate = \"idle\"\ncontains = \"x\"\n",
        )
        .expect_err("a newer engine's rules must not load here");
        assert_eq!(
            err,
            ManifestError::EngineTooNew {
                id: "future".to_string(),
                min_engine_version: 999,
            }
        );
        assert!(err.to_string().contains("future"));
        // This engine's own version always loads.
        assert!(
            Manifest::parse("id = \"t\"\nversion = \"1\"\nmin_engine_version = 1\n\n[[rules]]\nid = \"r\"\nstate = \"idle\"\ncontains = \"x\"\n")
                .is_ok()
        );
    }

    #[test]
    fn a_bundled_resume_table_names_its_own_agent_and_says_what_it_can_do() {
        // Every bundled manifest that offers a resume has to offer one that works: a
        // command line starting with a program name, and a session line that puts the id
        // somewhere. Read over the bundled set rather than over a list, so a manifest added
        // later is covered by having been added.
        let mut offered = 0;
        for manifest in builtin() {
            let Some(resume) = manifest.resume() else {
                continue;
            };
            offered += 1;
            assert!(
                !resume.session.is_empty() || !resume.latest.is_empty(),
                "{}: a [resume] table that offers neither line is the same as no table",
                manifest.id
            );
            for argv in [&resume.session, &resume.latest] {
                if argv.is_empty() {
                    continue;
                }
                assert!(
                    !argv[0].starts_with('-'),
                    "{}: {argv:?} starts with a flag rather than a program",
                    manifest.id
                );
            }
            if !resume.session.is_empty() {
                let built = resume.argv(Some("abc-123")).expect("a session line");
                assert!(
                    built.iter().any(|a| a.contains("abc-123")),
                    "{}: the id never reached the command line: {built:?}",
                    manifest.id
                );
                assert!(
                    !built.iter().any(|a| a.contains(SESSION_PLACEHOLDER)),
                    "{}: the placeholder survived substitution: {built:?}",
                    manifest.id
                );
            }
        }
        assert!(
            offered >= 3,
            "only {offered} agents can be carried across a restart, which proves little"
        );
    }

    #[test]
    fn an_unknown_id_falls_back_to_the_newest_conversation_rather_than_to_nothing() {
        let resume = Manifest::parse(
            "id = \"x\"\nversion = \"1\"\n[resume]\n\
             session = [\"x\", \"--resume\", \"{session}\"]\nlatest = [\"x\", \"-c\"]\n",
        )
        .expect("parse")
        .resume()
        .cloned()
        .expect("a table");
        assert_eq!(
            resume.argv(Some("id-7")),
            Some(vec!["x".into(), "--resume".into(), "id-7".into()])
        );
        assert_eq!(resume.argv(None), Some(vec!["x".into(), "-c".into()]));

        // An agent that can only be resumed by id, with no id in hand, has nothing to run.
        let only_by_id = Manifest::parse(
            "id = \"y\"\nversion = \"1\"\n[resume]\nsession = [\"y\", \"{session}\"]\n",
        )
        .expect("parse")
        .resume()
        .cloned()
        .expect("a table");
        assert_eq!(only_by_id.argv(None), None);
    }

    #[test]
    fn a_resume_table_that_cannot_do_what_it_says_is_refused_at_parse_time() {
        // A session line with no placeholder runs one command for every conversation.
        let err = Manifest::parse(
            "id = \"x\"\nversion = \"1\"\n[resume]\nsession = [\"x\", \"--resume\"]\n",
        )
        .expect_err("a session line that cannot name a session");
        assert!(matches!(err, ManifestError::BadResume { .. }), "{err}");
        assert!(err.to_string().contains("{session}"), "{err}");

        // A latest line with one would pass the placeholder through as an argument.
        let err = Manifest::parse(
            "id = \"x\"\nversion = \"1\"\n[resume]\nlatest = [\"x\", \"{session}\"]\n",
        )
        .expect_err("a latest line that needs a session");
        assert!(matches!(err, ManifestError::BadResume { .. }), "{err}");

        let err =
            Manifest::parse("id = \"x\"\nversion = \"1\"\n[resume]\nlatest = [\"x\", \" \"]\n")
                .expect_err("an empty argument");
        assert!(matches!(err, ManifestError::BadResume { .. }), "{err}");

        // And a manifest with no table at all is not an error, it is an agent that cannot
        // be carried across a restart.
        let none = Manifest::parse("id = \"x\"\nversion = \"1\"\n").expect("parse");
        assert!(none.resume().is_none());
    }

    #[test]
    fn an_override_changes_the_command_a_restore_runs() {
        with_clean_overrides(|| {
            let before = resume_argv("claude", None).expect("a bundled table");
            assert_eq!(before[0], "claude");

            let dir = tempfile::tempdir().expect("a directory");
            write_override(
                dir.path(),
                "claude.toml",
                "id = \"claude\"\nversion = \"9\"\n[resume]\n\
                 session = [\"my-claude\", \"--pick\", \"{session}\"]\n\
                 latest = [\"my-claude\", \"--newest\"]\n",
            );
            let outcome = reload_overrides(dir.path());
            assert!(outcome.errors.is_empty(), "{outcome:?}");

            assert_eq!(
                resume_argv("claude", None),
                Some(vec!["my-claude".into(), "--newest".into()]),
                "an override has to win, or the table is not configurable"
            );
            assert_eq!(
                resume_argv("claude", Some("k9")),
                Some(vec!["my-claude".into(), "--pick".into(), "k9".into()])
            );
            assert_eq!(resume_argv("no-such-agent", None), None);
        });
    }

    /// Override tests share one process-global table with every other test in this
    /// binary, so a test that installs one takes this exclusively and every test that
    /// reads a manifest takes it shared. A leaked or overlapping override quietly reroutes
    /// somebody else's manifests, which is a failure in a test that never mentioned
    /// overrides.
    static OVERRIDE_TEST_LOCK: RwLock<()> = RwLock::new(());

    fn with_clean_overrides<T>(run: impl FnOnce() -> T) -> T {
        let _held = OVERRIDE_TEST_LOCK.write().unwrap();
        OVERRIDES.write().unwrap().clear();
        let out = run();
        OVERRIDES.write().unwrap().clear();
        out
    }

    fn write_override(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write the override file");
    }

    #[test]
    fn a_good_override_changes_a_verdict_and_a_bad_one_keeps_the_old_rules() {
        with_clean_overrides(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            // Sanity: bundled claude reads the spinner as working.
            let spinner = ["\u{23f5} Cogitating… (12s · esc to interrupt)", ""];
            let before = verdict_unlocked("claude", &spinner).expect("bundled verdict");
            assert_eq!(before.state, AgentRuntimeState::Working);

            // A good override replaces the verdict for its agent only.
            write_override(
                dir.path(),
                "claude.toml",
                "id = \"claude\"\nversion = \"test\"\n\n[[rules]]\nid = \"always_idle\"\nstate = \"idle\"\npriority = 5000\ncontains = \"Cogitating\"\n",
            );
            let outcome = reload_overrides(dir.path());
            assert_eq!(outcome.loaded, vec!["claude".to_string()]);
            assert!(outcome.errors.is_empty());
            let after = verdict_unlocked("claude", &spinner).expect("override verdict");
            assert_eq!(after.rule, "always_idle");
            assert_eq!(after.state, AgentRuntimeState::Idle);

            // A bad file is reported and the previous rules keep serving.
            write_override(dir.path(), "claude.toml", "id = \"claude\"\n[[rules\n");
            let outcome = reload_overrides(dir.path());
            assert!(outcome.loaded.is_empty());
            assert_eq!(outcome.errors.len(), 1);
            assert_eq!(outcome.errors[0].0, "claude");
            let kept = verdict_unlocked("claude", &spinner).expect("kept verdict");
            assert_eq!(kept.rule, "always_idle", "a bad file keeps the old rules");

            // A filename that does not match its id is an error, not a rename.
            write_override(
                dir.path(),
                "codex.toml",
                "id = \"claude\"\nversion = \"test\"\n\n[[rules]]\nid = \"r\"\nstate = \"idle\"\ncontains = \"x\"\n",
            );
            let outcome = reload_overrides(dir.path());
            assert!(outcome.errors.iter().any(|(f, _)| f == "codex"));
        });
    }

    /// Deleting an override file is how a person takes an override back, so the next
    /// reload has to drop its rules, and a directory that is gone drops every one.
    #[test]
    fn a_deleted_override_file_stops_overriding() {
        with_clean_overrides(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            write_override(
                dir.path(),
                "zzl3.toml",
                "id = \"zzl3\"\nversion = \"test\"\n\n[[rules]]\nid = \"r\"\nstate = \"idle\"\ncontains = \"x\"\n",
            );
            reload_overrides(dir.path());
            assert!(for_program("zzl3").is_some());

            std::fs::remove_file(dir.path().join("zzl3.toml")).expect("remove");
            reload_overrides(dir.path());
            assert!(
                for_program("zzl3").is_none(),
                "a deleted file kept its rules"
            );

            write_override(
                dir.path(),
                "zzl3.toml",
                "id = \"zzl3\"\nversion = \"test\"\n\n[[rules]]\nid = \"r\"\nstate = \"idle\"\ncontains = \"x\"\n",
            );
            reload_overrides(dir.path());
            let gone = dir.path().to_path_buf();
            drop(dir);
            reload_overrides(&gone);
            assert!(
                for_program("zzl3").is_none(),
                "a removed directory kept its rules"
            );
        });
    }

    #[test]
    fn reloading_a_missing_directory_is_an_empty_success() {
        with_clean_overrides(|| {
            let outcome = reload_overrides(std::path::Path::new("/nonexistent-qoop-dir"));
            assert!(outcome.loaded.is_empty());
            assert!(outcome.errors.is_empty());
        });
    }
}

#[cfg(test)]
mod against_a_real_screen {
    use super::*;

    #[test]
    fn the_screen_a_stand_in_agent_draws_reads_as_blocked() {
        // The exact rows a terminal holds after the script in `interface.rs` has run: the
        // prompt the shell drew before it, then the dialog as Claude Code draws it: its rule,
        // the question, and the two choices with the pointer on the first.
        let screen: Vec<String> = [
            "sh-3.2$ /tmp/bin/claude",
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            " Do you want to proceed?",
            " \u{276F} 1. Yes",
            "   2. No",
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
            Some(qoop_domain::AgentRuntimeState::Blocked),
            "matched {:?}",
            verdict.as_ref().map(|v| &v.rule)
        );
    }

    /// The folder trust question is answered by moving the menu's pointer onto the trusting
    /// option and pressing enter: down from `No, exit` on Claude Code, enter alone where the
    /// pointer already stands on Codex, and nothing at all without a pointer to count from.
    #[test]
    fn the_trust_answer_walks_the_pointer_to_the_trusting_option() {
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let claude = lines(&[
            " Accessing workspace:",
            " /Users/me",
            "",
            " Quick safety check: Is this a project you created or one you trust?",
            "",
            " \u{276f} No, exit",
            "   Yes, I trust this folder",
            "",
            " Enter to confirm \u{b7} Esc to cancel",
        ]);
        let accept = rules_for("claude")
            .and_then(|m| m.accept_of("trust_folder").map(str::to_string))
            .expect("claude answers its trust question");
        let keys = |screen: &[String], accept: &str, app: bool| accept_keys(screen, accept, app);
        assert_eq!(
            keys(&claude, &accept, false).as_deref(),
            Some(&b"\x1b[B"[..])
        );
        // Application cursor keys: the arrow as the program asked for it.
        assert_eq!(
            keys(&claude, &accept, true).as_deref(),
            Some(&b"\x1bOB"[..])
        );
        let codex = lines(&[
            "  Folder access",
            "  Trust this folder? Codex can read, edit, and run files here.",
            "\u{203a} 1. Trust and continue",
            "  2. Quit",
            "  enter continue \u{b7} esc quit",
        ]);
        let accept = rules_for("codex")
            .and_then(|m| m.accept_of("trust_folder").map(str::to_string))
            .expect("codex answers its trust question");
        assert_eq!(keys(&codex, &accept, false).as_deref(), Some(&b"\r"[..]));
        // Pointer below the option, a blank row between: one option up, not two.
        let yes = "Yes, I trust this folder";
        let below = lines(&["   Yes, I trust this folder", "", " \u{276f} No, exit"]);
        assert_eq!(keys(&below, yes, false).as_deref(), Some(&b"\x1b[A"[..]));
        let unpointed = lines(&["   No, exit", "   Yes, I trust this folder"]);
        assert_eq!(keys(&unpointed, yes, false), None);
        // A quoted line is not a pointer.
        let quoted = lines(&["> No, exit", "  Yes, I trust this folder"]);
        assert_eq!(keys(&quoted, yes, false), None);
        assert_eq!(keys(&claude, "not on screen", false), None);
        // Once the pointer stands on the option, enter.
        let moved = lines(&["   No, exit", " \u{276f} Yes, I trust this folder"]);
        assert_eq!(keys(&moved, yes, false).as_deref(), Some(&b"\r"[..]));
        // Only a setup gate answers: a rule without an accept has none.
        assert!(rules_for("claude").is_some_and(|m| m.accept_of("first_run_dialog").is_none()));
    }
}

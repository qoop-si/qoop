//! Phase F1: replay spinner-to-composer transitions through both debounce rules.
//!
//! Fixture format (`crates/qoop-detect/tests/fixtures/*.txt`):
//!
//! ```text
//! # comment lines start with `#`
//! agent: claude
//! @@ 0
//! <screen lines of the frame at 0ms>
//! @@ 100 ## some title
//! <screen lines of the frame at 100ms with the OSC title `some title`>
//! ```
//!
//! The `## <title>` suffix sets the OSC title for its frame and every
//! following frame until changed; it lives on the `@@` line so a title can
//! never leak onto a neighbouring frame. A non-empty line outside a frame
//! that is none of the above is a parse error, not screen content: silently
//! swallowing a typo'd directive would fake the measurement.
//!
//! Frames are synthesized transitions built from the manifests' own verified
//! markers, not recordings: the agents are not installed or authed on the
//! machine that produced them, and claiming otherwise would be fabrication.
//! Each marker line already has a passing manifest test behind it; what is
//! synthesized is the TIMING (clean handoff, redraw tear, pause-and-resume).
//!
//! Two policies run over the same frames:
//!
//! - qoro: output settles 80ms after the last frame of a burst (a gap over
//!   80ms splits bursts); the last frame of each burst is evaluated once and
//!   published on change. This mirrors `DETECT_SETTLE` in the daemon.
//! - the polled schedule: the screen is read every 100ms; a Working-to-plain-Idle step
//!   needs three consecutive Idle reads (300ms, inside the 700ms cap) while a
//!   `visible_idle` Idle publishes at once. This mirrors the 100ms-x3 rule.
//!
//! Metric: published Working-to-Idle-to-Working flaps per fixture. The plan's
//! threshold keeps the 80ms settle unless the polled schedule shows strictly fewer flaps on
//! the same fixture, which the assertion below encodes: a failure here means
//! re-evaluate F1, not adjust the counting.
//!
//! Run all fixtures with `cargo test -p qoop-detect --test replay`, or one
//! with `REPLAY_FIXTURE=<name>.txt` in the environment. (A positional fixture
//! argument after `--` is not possible: libtest rejects unknown flags before
//! any test runs, so the selection travels by environment.)

use std::path::{Path, PathBuf};

use qoop_detect::{Manifest, OscContext};
use qoop_domain::AgentRuntimeState;

struct Frame {
    at: u64,
    lines: Vec<String>,
    title: String,
}

struct Fixture {
    agent: String,
    frames: Vec<Frame>,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn parse_fixture(path: &Path) -> Fixture {
    let text = std::fs::read_to_string(path).expect("read the fixture");
    let mut agent = String::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut title = String::new();
    let mut current: Option<(u64, Vec<String>)> = None;
    for line in text.lines() {
        if line.starts_with('#') {
        } else if let Some(name) = line.strip_prefix("agent:") {
            agent = name.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("@@") {
            if let Some((prev_at, lines)) = current.take() {
                frames.push(Frame {
                    at: prev_at,
                    lines,
                    title: title.clone(),
                });
            }
            let (at_part, title_part) = match rest.split_once("##") {
                Some((a, t)) => (a, Some(t)),
                None => (rest, None),
            };
            if let Some(t) = title_part {
                title = t.trim().to_string();
            }
            current = Some((
                at_part.trim().parse().expect("a millisecond timestamp"),
                Vec::new(),
            ));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        } else if !line.trim().is_empty() {
            panic!("{path:?}: line outside any frame: {line:?}");
        }
    }
    if let Some((at, lines)) = current.take() {
        frames.push(Frame { at, lines, title });
    }
    assert!(!agent.is_empty(), "{path:?} names no agent");
    assert!(!frames.is_empty(), "{path:?} holds no frames");
    Fixture { agent, frames }
}

fn raw_of(manifest: &Manifest, frame: &Frame) -> Option<qoop_detect::Verdict> {
    manifest.evaluate_with_osc(
        &frame.lines,
        &OscContext {
            title: &frame.title,
        },
    )
}

/// qoro: one evaluation per burst at 80ms past its last frame.
fn run_qoro(manifest: &Manifest, frames: &[Frame]) -> Vec<(u64, AgentRuntimeState)> {
    const SETTLE: u64 = 80;
    let mut published: Vec<(u64, AgentRuntimeState)> = Vec::new();
    let mut burst_last = 0usize;
    for i in 1..=frames.len() {
        let split = i == frames.len() || frames[i].at - frames[i - 1].at > SETTLE;
        if split {
            let frame = &frames[burst_last.max(i.saturating_sub(1))];
            let at = if i == frames.len() {
                frames[frames.len() - 1].at + SETTLE
            } else {
                frame.at + SETTLE
            };
            if let Some(v) = raw_of(manifest, frame)
                && published.last().is_none_or(|(_, s)| *s != v.state)
            {
                published.push((at, v.state));
            }
            burst_last = i;
        }
    }
    published
}

/// The polled schedule: 100ms reads, Working-to-plain-Idle confirmed three times in a row.
fn run_reference(manifest: &Manifest, frames: &[Frame]) -> Vec<(u64, AgentRuntimeState)> {
    let end = frames.last().map(|f| f.at).unwrap_or(0) + 800;
    let mut published: Vec<(u64, AgentRuntimeState)> = Vec::new();
    let mut pending_idle = 0u32;
    let mut t = 0u64;
    while t <= end {
        let frame = frames.iter().rev().find(|f| f.at <= t);
        let raw = frame.and_then(|f| raw_of(manifest, f));
        let current = published.last().map(|(_, s)| *s);
        // Copied out: the verdict is matched below and consulted after.
        let raw_state = raw.as_ref().map(|v| (v.state, v.visible_idle));
        match (current, raw_state) {
            (None, Some((state, _))) => {
                published.push((t, state));
                pending_idle = 0;
            }
            (Some(AgentRuntimeState::Working), Some((AgentRuntimeState::Idle, false))) => {
                pending_idle += 1;
                if pending_idle >= 3 {
                    published.push((t, AgentRuntimeState::Idle));
                    pending_idle = 0;
                }
            }
            (Some(cur), Some((state, _))) if cur != state => {
                published.push((t, state));
                pending_idle = 0;
            }
            _ => {
                if raw_state.is_none_or(|(state, _)| Some(state) == current) {
                    pending_idle = 0;
                }
            }
        }
        t += 100;
    }
    published
}

fn flaps(published: &[(u64, AgentRuntimeState)]) -> usize {
    published
        .windows(3)
        .filter(|w| {
            w[0].1 == AgentRuntimeState::Working
                && w[1].1 == AgentRuntimeState::Idle
                && w[2].1 == AgentRuntimeState::Working
        })
        .count()
}

fn manifest_for(agent: &str) -> Manifest {
    // Bundled only: overrides stay out of a measurement by construction, since
    // this binary never writes the override table.
    let src = match agent {
        "claude" => include_str!("../manifests/claude.toml"),
        "codex" => include_str!("../manifests/codex.toml"),
        "opencode" => include_str!("../manifests/opencode.toml"),
        "cursor" => include_str!("../manifests/cursor.toml"),
        "gemini" => include_str!("../manifests/gemini.toml"),
        "amp" => include_str!("../manifests/amp.toml"),
        "generic" => include_str!("../manifests/generic.toml"),
        other => panic!("no bundled manifest for fixture agent {other:?}"),
    };
    Manifest::parse(src).expect("a bundled manifest must parse")
}

#[test]
fn replay_every_transition_fixture_through_both_debounce_rules() {
    let dir = fixtures_dir();
    let single = std::env::var("REPLAY_FIXTURE").ok();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the fixtures directory exists")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    paths.sort();
    if let Some(name) = single {
        paths.retain(|p| p.file_name().is_some_and(|f| f == name.as_str()));
        assert!(!paths.is_empty(), "no fixture named {name:?}");
    }
    assert!(!paths.is_empty(), "no fixtures in {}", dir.display());

    let mut reference_fewer = Vec::new();
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let fixture = parse_fixture(path);
        let manifest = manifest_for(&fixture.agent);
        let qoro = run_qoro(&manifest, &fixture.frames);
        let reference = run_reference(&manifest, &fixture.frames);
        let (qf, hf) = (flaps(&qoro), flaps(&reference));
        println!(
            "{name}: agent={} qoro_flaps={qf} reference_flaps={hf}",
            fixture.agent
        );
        println!("  qoro : {qoro:?}");
        println!("  reference: {reference:?}");
        if hf < qf {
            reference_fewer.push(name);
        }
    }
    assert!(
        reference_fewer.is_empty(),
        "F1 threshold: the polled schedule shows strictly fewer flaps on \
         {reference_fewer:?}; re-evaluate the 80ms settle instead of keeping it"
    );
}

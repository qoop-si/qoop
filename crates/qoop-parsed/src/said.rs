//! What the person typed to agents: every message a human typed in any agent's sessions on this
//! computer, less what qoop typed for someone else (`typed.jsonl`), the blocks he pasted, and
//! what other agents sent. qass's nightly learning reads it (`qoop said`).

use std::path::Path;

use agentparse::{EntryKind, Roots};
use serde::Serialize;

/// One message he typed. `ts` is unix milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Said {
    pub ts: i64,
    pub agent: String,
    pub session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// `owner/name` of the GitHub repository its folder is in, by that folder's origin (filled
    /// in by the caller; a transcript names the branch, not the repository).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub text: String,
}

/// Words qoop typed into a terminal for someone else, as `typed.jsonl` keeps them: `at` in unix
/// milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Typed {
    pub at: i64,
    pub text: String,
}

/// How long after qoop typed a text the agent may record it: an agent started with words takes
/// a while to boot.
const TYPED_AFTER_MS: i64 = 120_000;
/// A clock a little ahead of the transcript's.
const TYPED_BEFORE_MS: i64 = 2_000;
/// Before `typed.jsonl` existed only the time of a write is known (`prompted.log`): a message
/// this soon after one is left out, his or not, since nothing says which.
const PROMPTED_AFTER_MS: i64 = 15_000;
/// A message this short is matched to what qoop typed only when it is the whole of it: "go"
/// inside a long prompt is not that prompt.
const SHORT: usize = 20;

/// What qoop's hooks put into every session of an agent in a qoop terminal (task.rs `RULES`
/// and the line beside each request), under its old name too.
const IN_QOOP: &[&str] = &[
    "You are running inside qoop",
    "You are running inside qoro",
    "qoop: real work gets a task",
    "qoop: this terminal's task #",
    "qoop: task #",
];

/// What a run nobody sat at writes of itself: Claude Code's `-p` (`"entrypoint":"sdk-cli"`, and
/// the SDKs' `sdk-ts`, `sdk-py`) and `codex exec`. An agent in a qoop terminal starts these, so
/// they carry qoop's lines too.
const HEADLESS: &[&str] = &["\"entrypoint\":\"sdk-", "\"originator\":\"codex_exec\""];

/// Whether he could have typed in this session: it ran in a qoop terminal (he talks to agents
/// there; a test in tmux did not) and someone sat at it, or it is qass's own (whose space is
/// told nothing).
// ponytail: the raw file is searched for the marks, as agentparse keeps neither; a typed
// `Session` field is the upgrade if a second reader needs them.
fn his(source: &agentparse::Source, cwd: Option<&str>, qass: Option<&Path>) -> bool {
    if qass
        .zip(cwd)
        .is_some_and(|(q, c)| Path::new(c).starts_with(q))
    {
        return true;
    }
    let agentparse::Source::File { path } = source else {
        return false;
    };
    let has = |bytes: &[u8], m: &str| bytes.windows(m.len()).any(|w| w == m.as_bytes());
    std::fs::read(path).is_ok_and(|bytes| {
        IN_QOOP.iter().any(|m| has(&bytes, m)) && !HEADLESS.iter().any(|m| has(&bytes, m))
    })
}

/// Every message he typed since `since` (unix ms), oldest first, in sessions that ran in a qoop
/// terminal or are qass's (`qass`: its folder). `typed`: what qoop typed; `prompted`: the times
/// of writes known only by their time (unix ms).
pub fn said(
    roots: &Roots,
    since: i64,
    typed: &[Typed],
    prompted: &[i64],
    qass: Option<&Path>,
) -> Vec<Said> {
    let (refs, _) = agentparse::discover(roots);
    let typed: Vec<(i64, String)> = typed.iter().map(|t| (t.at, norm(&t.text))).collect();
    let first_typed = typed.iter().map(|(at, _)| *at).min().unwrap_or(i64::MAX);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // A subagent's "user" is the agent that started it: only top sessions.
    for r in refs
        .iter()
        .filter(|r| r.parent_id.is_none() && r.updated_at.is_none_or(|u| u >= since))
    {
        let Ok(session) = agentparse::parse(r) else {
            continue;
        };
        if !his(&session.source, session.cwd.as_deref(), qass) {
            continue;
        }
        let git = session.git.as_ref();
        for entry in &session.entries {
            let (EntryKind::Message(m), Some(ts)) = (&entry.kind, entry.ts) else {
                continue;
            };
            // A queued prompt is said again when it is taken up: once is enough.
            if ts < since || !m.is_human() || m.phase.as_deref() == Some("queued") {
                continue;
            }
            let text = unpasted(&m.text());
            if text.trim().is_empty() || by_qoop(&norm(&text), ts, &typed) {
                continue;
            }
            if ts < first_typed
                && prompted
                    .iter()
                    .any(|p| ts >= *p && ts - p <= PROMPTED_AFTER_MS)
            {
                continue;
            }
            // A resumed or copied session holds the same message again.
            if !seen.insert((ts, text.clone())) {
                continue;
            }
            out.push(Said {
                ts,
                agent: format!("{:?}", session.agent).to_lowercase(),
                session: session.id.clone(),
                cwd: session.cwd.clone(),
                branch: git.and_then(|g| g.branch.clone()),
                remote: git.and_then(|g| g.remote.clone()),
                repo: None,
                text: crate::redact(text.trim()),
            });
        }
    }
    out.sort_by_key(|s| s.ts);
    out
}

/// Whether qoop typed this message: soon after a text it typed that it starts with, or that
/// holds it whole (an agent started with words takes them from its command line).
fn by_qoop(said: &str, ts: i64, typed: &[(i64, String)]) -> bool {
    typed.iter().any(|(at, text)| {
        ts >= at - TYPED_BEFORE_MS
            && ts - at <= TYPED_AFTER_MS
            && !text.is_empty()
            && (said == text
                || said.starts_with(text.as_str())
                || (said.chars().count() >= SHORT && text.contains(said)))
    })
}

/// Whitespace as one space, carriage returns gone: how a text reads whatever way it was typed.
fn norm(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The message less the blocks he pasted in (logs, documents, other people's words): what is
/// left is what he said himself.
fn unpasted(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("<pasted_content") {
        out.push_str(&rest[..open]);
        match rest[open..].find("</pasted_content>") {
            Some(close) => rest = &rest[open + close + "</pasted_content>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(at: i64, text: &str) -> (i64, String) {
        (at, norm(text))
    }

    #[test]
    fn what_qoop_typed_is_not_his() {
        let qass = [typed(
            1_000,
            "The usage limit that stopped you is cleared;\r\n resume the task you were on.",
        )];
        // As the agent records it: whitespace its own, seconds later.
        assert!(by_qoop(
            &norm("The usage limit that stopped you is cleared; resume the task you were on."),
            4_000,
            &qass
        ));
        // The same words long after, or before, are his.
        assert!(!by_qoop(
            &norm("The usage limit that stopped you is cleared; resume the task you were on."),
            1_000 + TYPED_AFTER_MS + 1,
            &qass
        ));
        assert!(!by_qoop(
            &norm("The usage limit that stopped you is cleared; resume the task you were on."),
            1_000 - TYPED_BEFORE_MS - 1,
            &qass
        ));
    }

    #[test]
    fn a_short_word_inside_a_prompt_is_still_his() {
        let launch = [typed(
            0,
            "claude 'go through the open PRs and merge the green ones'",
        )];
        assert!(!by_qoop("go", 1_000, &launch));
        assert!(by_qoop(
            &norm("go through the open PRs and merge the green ones"),
            9_000,
            &launch
        ));
        let exact = [typed(0, "go")];
        assert!(by_qoop("go", 500, &exact));
    }

    #[test]
    fn a_message_is_matched_on_its_start_when_it_is_longer_than_what_was_kept() {
        let kept = "x".repeat(2000);
        let long = format!("{kept} and more");
        assert!(by_qoop(&long, 10, &[typed(0, &kept)]));
    }

    #[test]
    fn only_a_session_someone_sat_at_in_a_qoop_terminal_or_qass_is_his() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            agentparse::Source::File { path }
        };
        let tab = file(
            "tab.jsonl",
            "{\"entrypoint\":\"cli\",\"hook\":\"You are running inside qoop, which keeps\"}\n",
        );
        let tmux = file(
            "tmux.jsonl",
            "{\"entrypoint\":\"cli\",\"text\":\"Reply with the word\"}\n",
        );
        let print = file(
            "p.jsonl",
            "{\"entrypoint\":\"sdk-cli\",\"hook\":\"You are running inside qoop\"}\n",
        );
        let exec = file(
            "exec.jsonl",
            "{\"originator\":\"codex_exec\",\"ctx\":\"qoop: task #4\"}\n",
        );
        assert!(his(&tab, None, None));
        assert!(!his(&tmux, Some("/w"), None));
        assert!(!his(&print, None, None));
        assert!(!his(&exec, None, None));
        let qass = Path::new("/home/x/qoop/qass");
        assert!(his(&tmux, Some("/home/x/qoop/qass"), Some(qass)));
        assert!(!his(&tmux, Some("/home/x/qoop/qassy"), Some(qass)));
        let db = agentparse::Source::Sqlite {
            path: dir.path().join("o.db"),
            session_id: "s".into(),
        };
        assert!(!his(&db, None, None));
    }

    #[test]
    fn pasted_blocks_are_not_his_words() {
        assert_eq!(
            unpasted(
                "look at this <pasted_content id=\"1\">a log\nline</pasted_content> and fix it"
            ),
            "look at this  and fix it"
        );
        assert_eq!(
            unpasted("<pasted_content id=\"2\">only a paste</pasted_content>").trim(),
            ""
        );
        assert_eq!(
            unpasted("cut <pasted_content id=\"3\">never closed"),
            "cut "
        );
        assert_eq!(unpasted("plain words"), "plain words");
    }
}

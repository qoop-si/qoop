//! herdr's metadata tokens: small name/value pairs a plugin keeps on a pane or a space with
//! `pane.report_metadata` and `workspace.report_metadata`, read back as `tokens` in pane, agent
//! and space info. Held by the host in memory, as herdr's server holds them: a host that
//! restarts starts without them, and a plugin reports them again.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::host::Refused;

/// Limits, as herdr sets them.
const KEYS_PER_REPORT: usize = 16;
const KEYS_PER_RESOURCE: usize = 32;
const KEY_LEN: usize = 32;
const VALUE_LEN: usize = 80;
const SOURCE_LEN: usize = 80;
const SEQUENCE_SOURCES: usize = 32;
const TTL_MAX_MS: u64 = 86_400_000;

/// What one pane or space holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tokens {
    /// Name to value and when it goes, if it was given a time to live.
    entries: BTreeMap<String, (String, Option<Instant>)>,
    /// The last `seq` each source sent with tokens, at most [`SEQUENCE_SOURCES`] sources: an
    /// older or equal one is dropped.
    sequences: BTreeMap<String, u64>,
}

impl Tokens {
    /// The tokens still alive at `now`.
    pub(crate) fn values(&self, now: Instant) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .filter(|(_, (_, until))| until.is_none_or(|u| u > now))
            .map(|(k, (v, _))| (k.clone(), v.clone()))
            .collect()
    }

    /// Drop what has expired by `now`. `true` when anything went.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, (_, until)| until.is_none_or(|u| u > now));
        self.entries.len() != before
    }

    fn fresh(&self, source: &str, seq: Option<u64>) -> bool {
        seq.is_none_or(|seq| self.sequences.get(source).is_none_or(|last| seq > *last))
    }

    /// Apply one report. `Ok(true)` when what is read back changed.
    fn apply(&mut self, report: Report, kind: &str, now: Instant) -> Result<bool, Refused> {
        if !self.fresh(&report.source, report.seq) {
            return Ok(false);
        }
        self.entries
            .retain(|_, (_, until)| until.is_none_or(|u| u > now));
        if let Some(patch) = &report.tokens {
            let mut keys: BTreeSet<&String> = self.entries.keys().collect();
            for (k, v) in patch {
                if v.is_some() {
                    keys.insert(k);
                } else {
                    keys.remove(k);
                }
            }
            if keys.len() > KEYS_PER_RESOURCE {
                return Err((
                    "metadata_token_limit",
                    format!("{kind} metadata may contain at most {KEYS_PER_RESOURCE} tokens"),
                ));
            }
        }
        // Only a report that carries tokens records its source, so reports of nothing else
        // cannot grow the map without end.
        if let (Some(seq), Some(_)) = (report.seq, &report.tokens) {
            if !self.sequences.contains_key(&report.source)
                && self.sequences.len() >= SEQUENCE_SOURCES
            {
                return Err((
                    "metadata_sequence_source_limit",
                    format!(
                        "{kind} metadata may track at most {SEQUENCE_SOURCES} sequenced sources"
                    ),
                ));
            }
            self.sequences.insert(report.source.clone(), seq);
        }
        let until = report.ttl.and_then(|ttl| now.checked_add(ttl));
        let mut changed = false;
        for (k, v) in report.tokens.into_iter().flatten() {
            changed |= match v {
                Some(v) => self.entries.insert(k, (v.clone(), until)) != Some((v, until)),
                None => self.entries.remove(&k).is_some(),
            };
        }
        Ok(changed)
    }
}

/// One report, checked.
#[derive(Debug, PartialEq)]
struct Report {
    source: String,
    seq: Option<u64>,
    ttl: Option<Duration>,
    /// `None` when the report carries no tokens; a `None` value clears that name.
    tokens: Option<BTreeMap<String, Option<String>>>,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refused {
    (code, message.into())
}

fn source_of(params: &Value, key: &str) -> Result<Option<String>, Refused> {
    let Some(raw) = params.get(key).and_then(Value::as_str) else {
        // `source` is a required field of herdr's request: without it the request is malformed.
        if key == "source" {
            return Err(refuse(
                "invalid_request",
                "invalid request: missing field `source`",
            ));
        }
        return Ok(None);
    };
    let value = raw.trim();
    let bad = |m: &str| refuse("invalid_metadata_source", m);
    if value.is_empty() {
        return Err(bad("metadata source must not be empty"));
    }
    if value.chars().count() > SOURCE_LEN {
        return Err(bad("metadata source must be 80 characters or fewer"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-'))
    {
        return Err(bad(
            "metadata source may contain only ASCII letters, digits, colon, dot, underscore, and hyphen",
        ));
    }
    Ok(Some(value.to_string()))
}

fn tokens_of(params: &Value) -> Result<Option<BTreeMap<String, Option<String>>>, Refused> {
    let Some(raw) = params.get("tokens").and_then(Value::as_object) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() > KEYS_PER_REPORT {
        return Err(refuse(
            "invalid_metadata_token",
            format!("a metadata report may update at most {KEYS_PER_REPORT} tokens"),
        ));
    }
    let mut out = BTreeMap::new();
    for (key, value) in raw {
        if key.is_empty()
            || key.len() > KEY_LEN
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(refuse(
                "invalid_metadata_token",
                format!("invalid metadata token key: {key}"),
            ));
        }
        // A blank value clears the name, as a null does.
        let value = value.as_str().and_then(|v| {
            let kept: String = v
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(VALUE_LEN)
                .collect();
            let kept = kept.trim().to_string();
            (!kept.is_empty()).then_some(kept)
        });
        out.insert(key.clone(), value);
    }
    Ok(Some(out))
}

fn ttl_of(params: &Value) -> Result<Option<Duration>, Refused> {
    match params.get("ttl_ms").and_then(Value::as_u64) {
        None => Ok(None),
        Some(0) => Err(refuse(
            "invalid_metadata_ttl",
            "metadata ttl_ms must be at least 1",
        )),
        Some(ms) if ms > TTL_MAX_MS => Err(refuse(
            "invalid_metadata_ttl",
            "metadata ttl_ms must be 86400000 or less",
        )),
        Some(ms) => Ok(Some(Duration::from_millis(ms))),
    }
}

/// A pane report, checked in herdr's order. Its title, display agent and state labels are
/// checked and then left: qoop shows a pane by its own state, not by what a plugin says.
fn pane_report(params: &Value) -> Result<Report, Refused> {
    let source = source_of(params, "source")?.unwrap_or_default();
    if source.is_empty() {
        return Err(refuse(
            "invalid_metadata_source",
            "metadata source must not be empty",
        ));
    }
    let tokens = tokens_of(params)?;
    let ttl = ttl_of(params)?;
    source_of(params, "applies_to_source")?;
    let set = |key: &str| match &params[key] {
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => false,
    };
    let clear = |key: &str| params[key] == true;
    let pairs = [
        ("title", "clear_title"),
        ("display_agent", "clear_display_agent"),
        ("state_labels", "clear_state_labels"),
    ];
    if pairs.iter().any(|(s, c)| set(s) && clear(c)) {
        return Err(refuse(
            "invalid_metadata_request",
            "cannot set and clear the same metadata field",
        ));
    }
    if tokens.is_none() && !pairs.iter().any(|(s, c)| set(s) || clear(c)) {
        return Err(refuse(
            "invalid_metadata_request",
            "missing metadata field to set or clear",
        ));
    }
    Ok(Report {
        source,
        seq: params.get("seq").and_then(Value::as_u64),
        ttl,
        tokens,
    })
}

/// A space report: tokens only, as herdr's.
fn space_report(params: &Value) -> Result<Report, Refused> {
    let source = source_of(params, "source")?.ok_or(refuse(
        "invalid_metadata_source",
        "metadata source must not be empty",
    ))?;
    let ttl = ttl_of(params)?;
    let tokens = tokens_of(params)?.ok_or(refuse(
        "invalid_metadata_token",
        "missing token to set or clear",
    ))?;
    Ok(Report {
        source,
        seq: params.get("seq").and_then(Value::as_u64),
        ttl,
        tokens: Some(tokens),
    })
}

/// `pane.report_metadata` against what a pane holds. `Ok(true)` when its tokens changed.
pub(crate) fn report_pane(
    held: &mut Tokens,
    params: &Value,
    now: Instant,
) -> Result<bool, Refused> {
    held.apply(pane_report(params)?, "pane", now)
}

/// `workspace.report_metadata` against what a space holds.
pub(crate) fn report_space(
    held: &mut Tokens,
    params: &Value,
    now: Instant,
) -> Result<bool, Refused> {
    held.apply(space_report(params)?, "workspace", now)
}

/// Put `tokens` on an info object when there are any, as herdr leaves an empty map out.
pub(crate) fn put(info: &mut Value, tokens: Option<&BTreeMap<String, String>>) {
    if let (Value::Object(info), Some(tokens)) = (info, tokens.filter(|t| !t.is_empty())) {
        let map: Map<String, Value> = tokens
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        info.insert("tokens".into(), Value::Object(map));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tokens_are_set_cleared_and_merged_per_pane() {
        let now = Instant::now();
        let mut held = Tokens::default();
        let report =
            |tokens: Value| json!({"pane_id": "w1:p1", "source": "t.sb", "tokens": tokens});
        assert_eq!(
            report_pane(&mut held, &report(json!({"a": "1", "b": "2"})), now),
            Ok(true)
        );
        assert_eq!(
            report_pane(&mut held, &report(json!({"a": "1"})), now),
            Ok(false),
            "same value"
        );
        assert_eq!(
            report_pane(&mut held, &report(json!({"a": null, "c": " 3 "})), now),
            Ok(true)
        );
        let other = json!({"source": "other", "tokens": {"d": "4"}});
        report_pane(&mut held, &other, now).expect("another source adds to the same map");
        let want: BTreeMap<String, String> = [("b", "2"), ("c", "3"), ("d", "4")]
            .map(|(k, v)| (k.into(), v.into()))
            .into();
        assert_eq!(held.values(now), want);
    }

    #[test]
    fn an_older_seq_is_dropped_and_a_ttl_expires() {
        let now = Instant::now();
        let mut held = Tokens::default();
        let at = |seq: u64, v: &str| json!({"source": "s", "seq": seq, "tokens": {"k": v}});
        report_pane(&mut held, &at(5, "five"), now).expect("first");
        assert_eq!(report_pane(&mut held, &at(5, "again"), now), Ok(false));
        assert_eq!(report_pane(&mut held, &at(4, "old"), now), Ok(false));
        assert_eq!(held.values(now)["k"], "five");
        report_pane(&mut held, &at(6, "six"), now).expect("newer");
        assert_eq!(held.values(now)["k"], "six");

        let brief = json!({"source": "s", "ttl_ms": 1000, "tokens": {"t": "x"}});
        report_pane(&mut held, &brief, now).expect("ttl");
        assert!(held.values(now).contains_key("t"));
        assert!(!held.values(now + Duration::from_secs(2)).contains_key("t"));
    }

    #[test]
    fn a_report_is_checked_as_herdr_checks_it() {
        let now = Instant::now();
        let mut held = Tokens::default();
        let code =
            |params: Value| report_pane(&mut Tokens::default(), &params, now).map_err(|e| e.0);
        assert_eq!(
            code(json!({"tokens": {"a": "1"}})),
            Err("invalid_request"),
            "no source at all is a malformed request"
        );
        assert_eq!(
            code(json!({"source": " ", "tokens": {"a": "1"}})),
            Err("invalid_metadata_source")
        );
        assert_eq!(
            code(json!({"source": "a b", "tokens": {"a": "1"}})),
            Err("invalid_metadata_source")
        );
        assert_eq!(
            code(json!({"source": "s"})),
            Err("invalid_metadata_request")
        );
        assert_eq!(
            code(json!({"source": "s", "tokens": {"a.b": "1"}})),
            Err("invalid_metadata_token")
        );
        assert_eq!(
            code(json!({"source": "s", "ttl_ms": 0, "tokens": {"a": "1"}})),
            Err("invalid_metadata_ttl")
        );
        assert_eq!(
            code(json!({"source": "s", "title": "x", "clear_title": true})),
            Err("invalid_metadata_request")
        );
        assert_eq!(
            code(json!({"source": "s", "title": "x"})),
            Ok(false),
            "presentation is accepted"
        );
        let many: Map<String, Value> = (0..17).map(|i| (format!("k{i}"), json!("v"))).collect();
        assert_eq!(
            code(json!({"source": "s", "tokens": many})),
            Err("invalid_metadata_token")
        );
        for round in 0..2 {
            let batch: Map<String, Value> = (0..16)
                .map(|i| (format!("r{round}k{i}"), json!("v")))
                .collect();
            report_pane(&mut held, &json!({"source": "s", "tokens": batch}), now).expect("32 fit");
        }
        let full = report_pane(
            &mut held,
            &json!({"source": "s", "tokens": {"one": "more"}}),
            now,
        );
        assert_eq!(full.map_err(|e| e.0), Err("metadata_token_limit"));

        let space =
            |params: Value| report_space(&mut Tokens::default(), &params, now).map_err(|e| e.0);
        assert_eq!(space(json!({"source": "s"})), Err("invalid_metadata_token"));
        assert_eq!(
            space(json!({"source": "s", "tokens": {"a": "1"}})),
            Ok(true)
        );
    }

    #[test]
    fn only_token_reports_record_their_source() {
        let now = Instant::now();
        let mut held = Tokens::default();
        for i in 0..40 {
            let title = json!({"source": format!("s{i}"), "seq": 1, "title": "t"});
            report_pane(&mut held, &title, now).expect("title only");
        }
        assert!(
            held.sequences.is_empty(),
            "title-only reports leave no trace"
        );
        for i in 0..SEQUENCE_SOURCES {
            let report = json!({"source": format!("s{i}"), "seq": 1, "tokens": {"k": "v"}});
            report_pane(&mut held, &report, now).expect("within the limit");
        }
        let one_more = json!({"source": "late", "seq": 1, "tokens": {"k": "v"}});
        assert_eq!(
            report_pane(&mut held, &one_more, now).map_err(|e| e.0),
            Err("metadata_sequence_source_limit")
        );
    }

    #[test]
    fn expiry_drops_what_is_due_and_says_so() {
        let now = Instant::now();
        let mut held = Tokens::default();
        let brief = json!({"source": "s", "ttl_ms": 1000, "tokens": {"t": "x"}});
        report_pane(&mut held, &brief, now).expect("ttl");
        assert!(!held.expire(now), "nothing due yet");
        assert!(held.expire(now + Duration::from_secs(2)));
        assert!(!held.expire(now + Duration::from_secs(3)), "already gone");
    }

    #[test]
    fn tokens_go_on_an_info_only_when_there_are_some() {
        let mut info = json!({"pane_id": "w1:p1"});
        put(&mut info, Some(&BTreeMap::new()));
        assert!(info.get("tokens").is_none());
        put(
            &mut info,
            Some(&[("a".to_string(), "1".to_string())].into()),
        );
        assert_eq!(info["tokens"], json!({"a": "1"}));
    }
}

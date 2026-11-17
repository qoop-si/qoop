//! `agent.view.set` and `agent.view.clear`: herdr's one agent view, a plugin's filter and sort
//! for the agent list in its sidebar. The host checks and keeps it and answers as herdr does;
//! qoop draws nothing from it (its left sidebar is its own), and `agent.list` is unaffected, as
//! in herdr.

use serde::Deserialize;
use serde_json::{Value, json};

use super::host::{Host, Refused, param};
use super::install;

const MAX_FILTER_DEPTH: usize = 8;
const MAX_FILTER_NODES: usize = 64;
const MAX_FILTER_VALUES: usize = 32;
const MAX_SORT_FIELDS: usize = 8;
const MAX_SOURCE_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 32;

/// The view held: its source and label, all a reply says of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Held {
    pub(crate) source: String,
    pub(crate) label: Option<String>,
}

#[derive(Deserialize)]
struct SetParams {
    source: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    filter: Option<Filter>,
    #[serde(default)]
    sort: Vec<Sort>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Filter {
    All {
        filters: Vec<Filter>,
    },
    Any {
        filters: Vec<Filter>,
    },
    Not {
        filter: Box<Filter>,
    },
    Eq {
        field: Field,
        value: ViewValue,
    },
    In {
        field: Field,
        values: Vec<ViewValue>,
    },
    Exists {
        field: Field,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Field {
    Builtin(Builtin),
    Token { token: String },
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Builtin {
    Status,
    WorkspaceId,
    TabId,
    PaneId,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(Deserialize)]
#[serde(untagged)]
// Read only for its shape: qoop filters nothing by it.
#[allow(dead_code)]
enum ViewValue {
    String(String),
    Bool(bool),
    Number(u64),
    Context { context: Context },
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Context {
    CurrentWorkspaceId,
    CurrentTabId,
}

#[derive(Deserialize)]
struct Sort {
    field: SortField,
    #[serde(default)]
    #[allow(dead_code)]
    order: Order,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SortField {
    #[allow(dead_code)]
    Builtin(SortBuiltin),
    Token {
        token: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortBuiltin {
    WorkspaceOrder,
    TabOrder,
    PaneOrder,
    Attention,
    Status,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum Order {
    #[default]
    Asc,
    Desc,
}

fn source_of(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty()
        || source.chars().count() > MAX_SOURCE_CHARS
        || !source
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-'))
    {
        return Err(format!(
            "agent view source must be non-empty, at most {MAX_SOURCE_CHARS} characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen"
        ));
    }
    Ok(source.to_string())
}

fn label_of(label: &str) -> Result<String, String> {
    let label: String = label.trim().chars().filter(|c| !c.is_control()).collect();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "agent view label must be non-empty and at most {MAX_LABEL_CHARS} characters"
        ));
    }
    Ok(label)
}

fn token(token: &str) -> Result<(), String> {
    let fits = !token.is_empty()
        && token.len() <= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if fits {
        Ok(())
    } else {
        Err(format!("invalid agent view token `{token}`"))
    }
}

fn field(field: &Field) -> Result<(), String> {
    match field {
        Field::Token { token: t } => token(t),
        Field::Builtin(_) => Ok(()),
    }
}

fn field_value(f: &Field, value: &ViewValue) -> Result<(), String> {
    field(f)?;
    match (f, value) {
        (Field::Builtin(Builtin::WorkspaceId), ViewValue::Context { context })
            if *context == Context::CurrentWorkspaceId =>
        {
            Ok(())
        }
        (Field::Builtin(Builtin::TabId), ViewValue::Context { context })
            if *context == Context::CurrentTabId =>
        {
            Ok(())
        }
        (_, ViewValue::Context { .. }) => {
            Err("agent view context type does not match the selected field".to_string())
        }
        (Field::Builtin(Builtin::Seen), ViewValue::Bool(_))
        | (Field::Builtin(Builtin::StateChangeSeq), ViewValue::Number(_)) => Ok(()),
        (Field::Builtin(Builtin::Status), ViewValue::String(s))
            if !["idle", "working", "blocked", "done", "unknown"].contains(&s.as_str()) =>
        {
            Err(format!("unknown agent status `{s}`"))
        }
        (
            Field::Builtin(
                Builtin::Status
                | Builtin::WorkspaceId
                | Builtin::TabId
                | Builtin::PaneId
                | Builtin::Agent,
            )
            | Field::Token { .. },
            ViewValue::String(_),
        ) => Ok(()),
        _ => Err("agent view value type does not match the selected field".to_string()),
    }
}

fn filter(f: &Filter, depth: usize, nodes: &mut usize) -> Result<(), String> {
    if depth > MAX_FILTER_DEPTH {
        return Err(format!(
            "agent view filter may be nested at most {MAX_FILTER_DEPTH} levels"
        ));
    }
    *nodes += 1;
    if *nodes > MAX_FILTER_NODES {
        return Err(format!(
            "agent view filter may contain at most {MAX_FILTER_NODES} nodes"
        ));
    }
    match f {
        Filter::All { filters } | Filter::Any { filters } => {
            if filters.is_empty() {
                return Err("agent view all/any filters must not be empty".to_string());
            }
            filters
                .iter()
                .try_for_each(|inner| filter(inner, depth + 1, nodes))
        }
        Filter::Not { filter: inner } => filter(inner, depth + 1, nodes),
        Filter::Eq { field: f, value } => field_value(f, value),
        Filter::In { field: f, values } => {
            if values.is_empty() || values.len() > MAX_FILTER_VALUES {
                return Err(format!(
                    "agent view in filters require 1 to {MAX_FILTER_VALUES} values"
                ));
            }
            values.iter().try_for_each(|v| field_value(f, v))
        }
        Filter::Exists { field: f } => field(f),
    }
}

/// A set request checked as herdr checks it: its source and label as they are kept.
fn checked(params: &Value) -> Result<Held, Refused> {
    let spec: SetParams = serde_json::from_value(params.clone())
        .map_err(|e| ("invalid_request", format!("invalid request: {e}")))?;
    let invalid = |message: String| ("invalid_agent_view", message);
    let source = source_of(&spec.source).map_err(invalid)?;
    let label = spec
        .label
        .as_deref()
        .map(label_of)
        .transpose()
        .map_err(invalid)?;
    if let Some(f) = &spec.filter {
        filter(f, 1, &mut 0).map_err(invalid)?;
    }
    if spec.sort.len() > MAX_SORT_FIELDS {
        return Err(invalid(format!(
            "agent view sort may contain at most {MAX_SORT_FIELDS} fields"
        )));
    }
    for sort in &spec.sort {
        if let SortField::Token { token: t } = &sort.field {
            token(t).map_err(invalid)?;
        }
    }
    Ok(Held { source, label })
}

/// herdr's `agent_view` answer for what is held.
fn reply(held: Option<&Held>) -> Value {
    let mut out = json!({"type": "agent_view", "active": held.is_some()});
    if let Some(held) = held {
        out["source"] = json!(held.source);
        if let Some(label) = &held.label {
            out["label"] = json!(label);
        }
    }
    out
}

impl Host {
    /// Whether a `plugin:<id>` source's plugin is here and agreed to: `Err` with herdr's code
    /// when not. Any other source is anybody's.
    fn owner_ok(&self, source: &str) -> Result<(), Refused> {
        let Some(id) = source.strip_prefix("plugin:") else {
            return Ok(());
        };
        if id.is_empty() {
            return Err((
                "invalid_agent_view",
                "plugin-owned agent view source has an invalid plugin id".to_string(),
            ));
        }
        if !install::installed(&self.home).iter().any(|i| i == id) {
            return Err(("plugin_not_found", "plugin not found".to_string()));
        }
        if install::admitted(&self.home, id).is_err() {
            return Err(("plugin_disabled", "plugin is disabled".to_string()));
        }
        Ok(())
    }

    /// `agent.view.set` and `agent.view.clear`. A view a plugin owns is dropped once that
    /// plugin is no longer installed and agreed to, as herdr drops it on an unlink or a
    /// disable; it is noticed here, at the next call.
    pub(super) fn agent_view(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        let held = self.state.lock().unwrap().agent_view.clone();
        if held
            .as_ref()
            .is_some_and(|h| self.owner_ok(&h.source).is_err())
        {
            self.state.lock().unwrap().agent_view = None;
        }
        if method == "agent.view.set" {
            let view = checked(params)?;
            self.owner_ok(&view.source)?;
            let out = reply(Some(&view));
            self.state.lock().unwrap().agent_view = Some(view);
            return Ok(out);
        }
        let source = param(params, "source")
            .map(source_of)
            .transpose()
            .map_err(|m| ("invalid_agent_view", m))?;
        let mut state = self.state.lock().unwrap();
        let matches = |h: &Held| source.as_ref().is_none_or(|s| *s == h.source);
        if state.agent_view.as_ref().is_some_and(matches) {
            state.agent_view = None;
        }
        Ok(reply(state.agent_view.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(source: &str) -> Value {
        json!({"source": source, "label": " working ",
               "filter": {"op": "eq", "field": "status", "value": "working"},
               "sort": [{"field": "attention", "order": "desc"}, {"field": {"token": "usage"}}]})
    }

    #[test]
    fn a_view_is_checked_as_herdr_checks_it() {
        let held = checked(&working("example.views")).expect("valid");
        assert_eq!(
            held,
            Held {
                source: "example.views".into(),
                label: Some("working".into())
            }
        );
        let code = |params: Value| checked(&params).err().map(|(code, _)| code);
        assert_eq!(
            code(json!({"source": "bad source"})),
            Some("invalid_agent_view")
        );
        assert_eq!(
            code(json!({"source": "a".repeat(121)})),
            Some("invalid_agent_view")
        );
        assert_eq!(
            code(json!({"source": "s", "label": "x".repeat(33)})),
            Some("invalid_agent_view")
        );
        assert_eq!(
            code(json!({"source": "s", "filter": {"op": "any", "filters": []}})),
            Some("invalid_agent_view")
        );
        assert_eq!(
            code(
                json!({"source": "s", "filter": {"op": "eq", "field": "status", "value": "busy"}})
            ),
            Some("invalid_agent_view"),
            "an unknown status"
        );
        assert_eq!(
            code(json!({"source": "s", "filter": {"op": "eq", "field": "seen", "value": "yes"}})),
            Some("invalid_agent_view"),
            "a value of the wrong type"
        );
        assert_eq!(
            code(
                json!({"source": "s", "filter": {"op": "eq", "field": "tab_id",
                                                  "value": {"context": "current_workspace_id"}}})
            ),
            Some("invalid_agent_view")
        );
        let mut deep = json!({"op": "exists", "field": "agent"});
        for _ in 0..8 {
            deep = json!({"op": "not", "filter": deep});
        }
        assert_eq!(
            code(json!({"source": "s", "filter": deep})),
            Some("invalid_agent_view"),
            "nine levels"
        );
        let many: Vec<Value> = (0..64)
            .map(|_| json!({"op": "exists", "field": "agent"}))
            .collect();
        assert_eq!(
            code(json!({"source": "s", "filter": {"op": "all", "filters": many}})),
            Some("invalid_agent_view"),
            "65 nodes"
        );
        let values: Vec<Value> = (0..33).map(|i| json!(format!("w{i}"))).collect();
        assert_eq!(
            code(
                json!({"source": "s", "filter": {"op": "in", "field": "workspace_id",
                                                  "values": values}})
            ),
            Some("invalid_agent_view")
        );
        let sorts: Vec<Value> = (0..9).map(|_| json!({"field": "status"})).collect();
        assert_eq!(
            code(json!({"source": "s", "sort": sorts})),
            Some("invalid_agent_view")
        );
        assert_eq!(
            code(json!({"source": "s", "sort": [{"field": {"token": "a b"}}]})),
            Some("invalid_agent_view")
        );
        assert_eq!(
            code(json!({"source": "s", "filter": {"op": "nope"}})),
            Some("invalid_request"),
            "a shape herdr cannot read"
        );
    }

    #[test]
    fn the_reply_is_herdrs_agent_view() {
        assert_eq!(reply(None), json!({"type": "agent_view", "active": false}));
        assert_eq!(
            reply(Some(&Held {
                source: "s".into(),
                label: None
            })),
            json!({"type": "agent_view", "active": true, "source": "s"})
        );
    }
}

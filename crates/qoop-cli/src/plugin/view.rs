//! `agent.view.set` and `agent.view.clear`: herdr's one agent view, a plugin's filter and sort
//! for the agent list in its sidebar. The host checks and keeps it and answers as herdr does,
//! and works out the order it gives the agents (`qoop.view`), which the interface's spaces
//! tree follows. `agent.list` is unaffected, as in herdr.

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

/// The view held: its source and label, which a reply says, and the request it came in, whose
/// filter and sort order the agents.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Held {
    pub(crate) source: String,
    pub(crate) label: Option<String>,
    pub(crate) spec: Value,
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
    order: Order,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SortField {
    Builtin(SortBuiltin),
    Token { token: String },
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
    Ok(Held {
        source,
        label,
        spec: params.clone(),
    })
}

/// One agent as a view reads it: herdr's fields, from the host's agent list.
#[derive(Clone, Debug, Default)]
pub(crate) struct Entry {
    pub(crate) terminal: u32,
    pub(crate) status: String,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: String,
    pub(crate) pane_id: String,
    pub(crate) agent: Option<String>,
    pub(crate) seq: Option<u64>,
    pub(crate) tokens: std::collections::BTreeMap<String, String>,
    /// The space's place in the list, from 0; the tab's number in its space, from 1; the
    /// pane's number (its terminal), as herdr numbers them.
    pub(crate) workspace_order: u64,
    pub(crate) tab_order: Option<u64>,
    pub(crate) pane_order: u64,
}

/// What a view compares, ordered as herdr orders its values.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Eval {
    Str(String),
    Bool(bool),
    Num(u64),
}

impl Entry {
    /// herdr's `seen`: an agent whose finish nobody has looked at yet reads `done`.
    fn seen(&self) -> bool {
        self.status != "done"
    }

    /// herdr's attention rank: blocked, then done unseen, working, idle, the rest.
    fn attention(&self) -> u64 {
        match self.status.as_str() {
            "blocked" => 4,
            "done" => 3,
            "working" => 2,
            "idle" => 1,
            _ => 0,
        }
    }

    fn field(&self, field: &Field) -> Option<Eval> {
        let text = |s: &str| Some(Eval::Str(s.to_string()));
        match field {
            Field::Token { token } => self.tokens.get(token).and_then(|v| text(v)),
            Field::Builtin(b) => match b {
                Builtin::Status => text(&self.status),
                Builtin::WorkspaceId => text(&self.workspace_id),
                Builtin::TabId => text(&self.tab_id),
                Builtin::PaneId => text(&self.pane_id),
                Builtin::Agent => self.agent.as_deref().and_then(text),
                Builtin::Seen => Some(Eval::Bool(self.seen())),
                Builtin::StateChangeSeq => self.seq.map(Eval::Num),
            },
        }
    }

    fn sorted_by(&self, field: &SortField) -> Option<Eval> {
        match field {
            SortField::Token { token } => self.tokens.get(token).map(|v| Eval::Str(v.clone())),
            SortField::Builtin(b) => match b {
                SortBuiltin::WorkspaceOrder => Some(Eval::Num(self.workspace_order)),
                SortBuiltin::TabOrder => self.tab_order.map(Eval::Num),
                SortBuiltin::PaneOrder => Some(Eval::Num(self.pane_order)),
                SortBuiltin::Attention => Some(Eval::Num(self.attention())),
                SortBuiltin::Status => Some(Eval::Str(self.status.clone())),
                SortBuiltin::Agent => self.agent.clone().map(Eval::Str),
                SortBuiltin::Seen => Some(Eval::Bool(self.seen())),
                SortBuiltin::StateChangeSeq => self.seq.map(Eval::Num),
            },
        }
    }
}

/// An entry from herdr's `AgentInfo` as the host lists it, with the space's place in `spaces`
/// and the tab's number from `tabs` (herdr's `TabInfo`s).
fn entry_of(
    agent: &Value,
    spaces: &[qoop_runtime::control::WorkspaceRow],
    tabs: &[Value],
) -> Option<Entry> {
    let text = |key: &str| agent[key].as_str().map(str::to_string);
    let workspace_id = text("workspace_id")?;
    let tab_id = text("tab_id")?;
    Some(Entry {
        terminal: text("terminal_id")?.parse().ok()?,
        status: text("agent_status").unwrap_or_else(|| "unknown".into()),
        pane_id: text("pane_id")?,
        agent: text("agent"),
        seq: agent["state_change_seq"].as_u64(),
        tokens: agent["tokens"]
            .as_object()
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        workspace_order: spaces
            .iter()
            .position(|s| super::translate::Id::Workspace(s.id).to_string() == workspace_id)
            .unwrap_or(usize::MAX) as u64,
        tab_order: tabs
            .iter()
            .find(|t| t["tab_id"] == tab_id.as_str())
            .and_then(|t| t["number"].as_u64()),
        pane_order: text("terminal_id")?.parse().ok()?,
        workspace_id,
        tab_id,
    })
}

/// The space and tab on screen, which a view's `current_*` values name.
#[derive(Clone, Debug, Default)]
pub(crate) struct Current {
    pub(crate) workspace_id: Option<String>,
    pub(crate) tab_id: Option<String>,
}

fn operand(current: &Current, value: &ViewValue) -> Option<Eval> {
    match value {
        ViewValue::String(s) => Some(Eval::Str(s.clone())),
        ViewValue::Bool(b) => Some(Eval::Bool(*b)),
        ViewValue::Number(n) => Some(Eval::Num(*n)),
        ViewValue::Context { context } => match context {
            Context::CurrentWorkspaceId => current.workspace_id.clone().map(Eval::Str),
            Context::CurrentTabId => current.tab_id.clone().map(Eval::Str),
        },
    }
}

fn matches(current: &Current, entry: &Entry, f: &Filter) -> bool {
    match f {
        Filter::All { filters } => filters.iter().all(|f| matches(current, entry, f)),
        Filter::Any { filters } => filters.iter().any(|f| matches(current, entry, f)),
        Filter::Not { filter } => !matches(current, entry, filter),
        Filter::Eq { field, value } => entry.field(field) == operand(current, value),
        Filter::In { field, values } => {
            let actual = entry.field(field);
            values.iter().any(|v| actual == operand(current, v))
        }
        Filter::Exists { field } => entry.field(field).is_some(),
    }
}

/// The agents a view shows, in its order: those its filter keeps, sorted by its keys in turn
/// (a value before none; `desc` the other way), ties kept in the list's own order, as herdr
/// sorts its panel.
pub(crate) fn ordered(spec: &Value, current: &Current, entries: &[Entry]) -> Vec<u32> {
    let Ok(spec) = serde_json::from_value::<SetParams>(spec.clone()) else {
        return entries.iter().map(|e| e.terminal).collect();
    };
    let mut kept: Vec<&Entry> = entries
        .iter()
        .filter(|e| spec.filter.as_ref().is_none_or(|f| matches(current, e, f)))
        .collect();
    kept.sort_by(|a, b| {
        for sort in &spec.sort {
            let order = match (a.sorted_by(&sort.field), b.sorted_by(&sort.field)) {
                (Some(x), Some(y)) if matches!(sort.order, Order::Desc) => y.cmp(&x),
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            };
            if order.is_ne() {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    });
    kept.iter().map(|e| e.terminal).collect()
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

    /// `qoop.view`, what the interface asks of plugins every few seconds: whether a plugin
    /// here has a link handler (`links`), and the held agent view as the agents it shows in its
    /// order and the agents its filter leaves out (`active: false` with none held, or its
    /// plugin gone).
    pub(super) fn view_order(&self) -> Value {
        let links = super::link::any_handler(&self.home);
        let held = self.state.lock().unwrap().agent_view.clone();
        let Some(held) = held.filter(|h| self.owner_ok(&h.source).is_ok()) else {
            self.state.lock().unwrap().agent_view = None;
            return json!({"active": false, "links": links});
        };
        let (order, hidden) = self.with_view(|v| {
            let current = v.focused.and_then(|t| super::host::pane_of(v.panes(), t));
            let text = |key: &str| {
                current
                    .as_ref()
                    .and_then(|p| p[key].as_str().map(String::from))
            };
            let current = Current {
                workspace_id: text("workspace_id"),
                tab_id: text("tab_id"),
            };
            let tabs = v.tab_infos();
            let entries: Vec<Entry> = v
                .agents()
                .iter()
                .filter_map(|a| entry_of(a, v.spaces, &tabs))
                .collect();
            let order = ordered(&held.spec, &current, &entries);
            let hidden: Vec<u32> = entries
                .iter()
                .map(|e| e.terminal)
                .filter(|t| !order.contains(t))
                .collect();
            (order, hidden)
        });
        json!({"active": true, "source": held.source, "order": order, "hidden": hidden,
               "links": links})
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
                label: Some("working".into()),
                spec: working("example.views"),
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
    fn a_view_filters_and_sorts_the_agents_as_herdr_does() {
        let entry = |terminal: u32, status: &str, ws: u64, quota: Option<&str>| Entry {
            terminal,
            status: status.into(),
            workspace_id: format!("w{}", ws + 1),
            tab_id: format!("w{}:t{terminal}", ws + 1),
            pane_id: format!("w{}:p{terminal}", ws + 1),
            agent: Some("claude".into()),
            seq: Some(u64::from(terminal)),
            tokens: quota
                .map(|q| [("quota".to_string(), q.to_string())].into())
                .unwrap_or_default(),
            workspace_order: ws,
            tab_order: Some(1),
            pane_order: u64::from(terminal),
        };
        let agents = [
            entry(1, "idle", 0, Some("40")),
            entry(2, "working", 0, None),
            entry(3, "blocked", 1, Some("90")),
            entry(4, "done", 1, Some("10")),
        ];
        let here = Current {
            workspace_id: Some("w2".into()),
            tab_id: None,
        };
        let order = |spec: Value| ordered(&spec, &here, &agents);
        assert_eq!(
            order(json!({"source": "s"})),
            [1, 2, 3, 4],
            "no keys: the list's own order"
        );
        assert_eq!(
            order(json!({"source": "s", "sort": [{"field": "attention", "order": "desc"}]})),
            [3, 4, 2, 1]
        );
        assert_eq!(
            order(json!({"source": "s", "sort": [{"field": {"token": "quota"}, "order": "desc"}]})),
            [3, 1, 4, 2],
            "one with no value goes last, either way"
        );
        assert_eq!(
            order(
                json!({"source": "s", "filter": {"op": "eq", "field": "workspace_id",
                                                     "value": {"context": "current_workspace_id"}}})
            ),
            [3, 4]
        );
        assert_eq!(
            order(json!({"source": "s", "filter": {"op": "not", "filter":
                         {"op": "in", "field": "status", "values": ["idle", "done"]}}})),
            [2, 3]
        );
        assert_eq!(
            order(
                json!({"source": "s", "filter": {"op": "exists", "field": {"token": "quota"}},
                         "sort": [{"field": "seen"}, {"field": "pane_order", "order": "desc"}]})
            ),
            [4, 3, 1],
            "false before true, then the next key"
        );
    }

    #[test]
    fn the_reply_is_herdrs_agent_view() {
        assert_eq!(reply(None), json!({"type": "agent_view", "active": false}));
        assert_eq!(
            reply(Some(&Held {
                source: "s".into(),
                label: None,
                spec: json!({}),
            })),
            json!({"type": "agent_view", "active": true, "source": "s"})
        );
    }
}

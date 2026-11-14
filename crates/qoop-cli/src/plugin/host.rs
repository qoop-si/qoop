//! `qoop plugin host`: the process herdr plugins talk to, in herdr's NDJSON on herdr's
//! socket path, and the one that runs their `[[events]]` and `[[startup]]` commands.
//!
//! An observer client of the daemon: it never keeps the daemon up, reads its link and
//! acknowledges renders, follows a live-upgrade exec by connecting again, and leaves when the
//! daemon has gone for good. Started lazily, once a plugin is installed, by the interface and
//! by `install`/`link`; never restarted in a loop (ADR-018).
//!
//! It owns the herdr view of qoop: which tab key each split tree keeps, each pane's revision,
//! and the events a change between two listings makes. Listings come from a 2 s poll, from the
//! daemon's `AgentChanged` and `Resync`, and from every request that reads them, all applied
//! one at a time so no listing is applied over a newer one.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qoop_pty::sys::ProcRow;
use qoop_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, EventMessage, Pane, WorkspaceRow,
};
use qoop_runtime::paths;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

use super::install::{self, Admitted, Home};
use super::manifest::{self, Hook};
use super::translate::{self, Id, Rect, Tab, View};
use crate::supervise::Link;

/// How often the host lists the daemon when nothing told it to.
const POLL: Duration = Duration::from_secs(2);
/// How long an `[[events]]` command may run before its process group is killed.
const HANDLER_LIMIT: Duration = Duration::from_secs(60);
/// How long the host waits for a daemon that went away to answer again (a live upgrade
/// re-executes in place) before it leaves too.
const RECONNECT: Duration = Duration::from_secs(10);
/// How long a process table read is reused.
const PROCS_FRESH: Duration = Duration::from_millis(500);
/// The area layouts are drawn in. qoop keeps no pane sizes, so rectangles are the split
/// tree's proportions of this, not cells on anyone's screen.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 200,
    height: 50,
};
/// How many events a subscriber may fall behind before it is told some were lost.
const BACKLOG: usize = 1024;
/// How long a listing stays fresh enough for a request, and how long a burst of daemon events
/// is let settle before one listing reads them all.
const FRESH: Duration = Duration::from_millis(150);
const COALESCE: Duration = Duration::from_millis(150);
/// How many `[[events]]` commands of one plugin run at once.
const HANDLERS_PER_PLUGIN: usize = 8;
/// How big a plugin's host log grows before it is moved aside.
const LOG_CAP: u64 = 1 << 20;

/// The socket's name, beside the daemon's in the session's verified 0700 directory.
pub(crate) const SOCKET: &str = "herdr.sock";

/// Where the host listens: what the plugin was told, else the session's own path. Nothing is
/// created here.
pub(crate) fn socket_path() -> PathBuf {
    std::env::var_os("HERDR_SOCKET_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::session_dir(&paths::base_location(), "default").join(SOCKET))
}

/// The methods C1 answers. The rest of `translate::METHODS` is [`LATER`]; a test holds the two
/// to exactly `METHODS`, so the unknown-method text never names what is not served.
const SERVED: &[&str] = &[
    "ping",
    "session.snapshot",
    "pane.list",
    "pane.get",
    "pane.current",
    "pane.read",
    "pane.layout",
    "pane.process_info",
    "pane.resize",
    "pane.report_metadata",
    "tab.list",
    "tab.get",
    "workspace.list",
    "workspace.get",
    "workspace.report_metadata",
    "agent.list",
    "agent.get",
    "plugin.list",
    "plugin.action.list",
    "server.reload_config",
    "events.subscribe",
];

/// Methods a pinned plugin calls that write, which land with the next part of the host.
// ponytail: refused as not served yet until C2 wires the writes.
#[cfg(test)]
const LATER: &[&str] = &[
    "pane.split",
    "pane.close",
    "pane.focus",
    "pane.send_text",
    "pane.send_input",
    "pane.rename",
    "pane.zoom",
    "tab.create",
    "tab.rename",
    "tab.focus",
    "tab.close",
    "workspace.create",
    "workspace.focus",
    "workspace.rename",
    "workspace.close",
    "agent.focus",
    "agent.prompt",
    "agent.rename",
    "notification.show",
    "plugin.action.invoke",
    "plugin.pane.open",
    "plugin.pane.close",
    "popup.close",
];

/// One change between two listings, by its dotted name. Written as the stream spells it for a
/// subscriber and as `EventKind` spells it for a handler.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Event {
    pub(crate) dotted: &'static str,
    /// The pane it is about, for a per-pane subscription.
    pub(crate) pane: Option<String>,
    pub(crate) data: Value,
    /// What a handler of it is told it is about: `HERDR_PLUGIN_CONTEXT_JSON` and the matching
    /// variables, worked out when the event is made, against the listing it was in.
    pub(crate) scope: Scope,
}

impl Event {
    fn new(dotted: &'static str, pane: Option<String>, data: Value) -> Self {
        Self {
            dotted,
            pane,
            data,
            scope: Scope::default(),
        }
    }

    /// The line a subscriber reads.
    fn for_subscriber(&self) -> Option<Value> {
        let name = translate::written(translate::SUBSCRIPTIONS, self.dotted)?;
        Some(translate::event(name, self.data.clone()))
    }

    /// The hook name and the JSON a handler is given.
    fn for_hook(&self) -> Option<(&'static str, Value)> {
        let name = translate::written(translate::HOOK_EVENTS, self.dotted)?;
        Some((name, translate::event(name, self.data.clone())))
    }
}

/// One listing of the daemon, with the host's tab keys applied.
#[derive(Clone, Default)]
struct Listing {
    spaces: Vec<WorkspaceRow>,
    rows: Vec<AgentRow>,
    tabs: Vec<Tab>,
}

/// The herdr view of qoop the host keeps between listings.
#[derive(Default)]
pub(crate) struct State {
    now: Listing,
    /// Per pane, raised on every change to its row and never reset: a reset reads as a new
    /// pane to a plugin that compares them.
    revisions: BTreeMap<u32, u64>,
    /// What each row's revision is compared on.
    seen: BTreeMap<u32, Value>,
    /// The terminal the interface shows, once it says.
    focused: Option<u32>,
    /// Whether a listing has been applied: the first one is the starting point, not news.
    primed: bool,
}

/// The parts of a row whose change is a new revision.
fn compared(row: &AgentRow) -> Value {
    json!([
        row.workspace,
        row.state,
        row.agent,
        row.program,
        row.name,
        row.agent_title,
        row.cwd,
        row.generation,
        row.agent_session,
    ])
}

/// Give each fresh tab the key it had: the key of any of its leaves in the last listing, so a
/// tab whose first terminal closed keeps its id. A tab none of whose leaves were seen gets its
/// first leaf, unless that number is some other tab's key.
fn keyed(mut fresh: Vec<Tab>, old: &[Tab]) -> Vec<Tab> {
    let key_of: BTreeMap<u32, u32> = old
        .iter()
        .flat_map(|t| t.leaves.iter().map(move |leaf| (*leaf, t.key)))
        .collect();
    let mut used: BTreeSet<u32> = BTreeSet::new();
    for tab in &mut fresh {
        let kept = tab
            .leaves
            .iter()
            .filter_map(|leaf| key_of.get(leaf))
            .copied()
            .find(|k| !used.contains(k));
        let taken = |k: &u32| used.contains(k) || key_of.values().any(|old| old == k);
        // A first leaf whose number is still some tab's key (a terminal number used again)
        // takes the first number nobody holds.
        tab.key = kept
            .or_else(|| tab.leaves.iter().copied().find(|leaf| !taken(leaf)))
            .or_else(|| (1..).find(|k| !taken(k)))
            .unwrap_or(tab.leaves[0]);
        used.insert(tab.key);
    }
    fresh
}

impl State {
    /// Apply a listing and say what changed since the last one.
    pub(crate) fn update(
        &mut self,
        spaces: Vec<WorkspaceRow>,
        rows: Vec<AgentRow>,
        splits: &[Pane],
    ) -> Vec<Event> {
        let tabs = keyed(translate::tabs(&rows, splits), &self.now.tabs);
        for row in &rows {
            let now = compared(row);
            if self.seen.get(&row.terminal) != Some(&now) {
                *self.revisions.entry(row.terminal).or_insert(0) += 1;
                self.seen.insert(row.terminal, now);
            }
        }
        let old = std::mem::replace(&mut self.now, Listing { spaces, rows, tabs });
        if !std::mem::replace(&mut self.primed, true) {
            return Vec::new();
        }
        let mut events = self.diff(&old);
        for event in &mut events {
            event.scope = scope_of(event, &self.now, &old);
        }
        events
    }

    pub(crate) fn view(&self) -> View<'_> {
        self.view_of(&self.now)
    }

    fn view_of<'a>(&'a self, listing: &'a Listing) -> View<'a> {
        View {
            spaces: &listing.spaces,
            rows: &listing.rows,
            tabs: &listing.tabs,
            focused: self.focused,
            revisions: &self.revisions,
        }
    }

    fn diff(&self, old: &Listing) -> Vec<Event> {
        let new = &self.now;
        let (view, before) = (self.view(), self.view_of(old));
        let find = |list: &[Value], key: &str, id: &str| {
            list.iter()
                .find(|v| v[key] == id)
                .cloned()
                .unwrap_or(Value::Null)
        };
        let (workspaces, tabs, panes) = (view.workspace_infos(), view.tab_infos(), view.panes());
        let old_tabs = before.tab_infos();
        let ws = |id: i64| Id::Workspace(id).to_string();
        let pane_id = |row: &AgentRow| {
            Id::Pane {
                workspace: row.workspace,
                terminal: row.terminal,
            }
            .to_string()
        };
        let key_of = |listing: &Listing, t: u32| {
            listing
                .tabs
                .iter()
                .find(|tab| tab.leaves.contains(&t))
                .map(|tab| tab.key)
        };
        let mut out = Vec::new();

        let common: Vec<i64> = new
            .spaces
            .iter()
            .map(|s| s.id)
            .filter(|id| old.spaces.iter().any(|o| o.id == *id))
            .collect();
        let old_common: Vec<i64> = old
            .spaces
            .iter()
            .map(|s| s.id)
            .filter(|id| common.contains(id))
            .collect();
        for (i, space) in new.spaces.iter().enumerate() {
            let id = ws(space.id);
            match old.spaces.iter().find(|o| o.id == space.id) {
                None => out.push(Event::new(
                    "workspace.created",
                    None,
                    json!({"workspace": find(&workspaces, "workspace_id", &id)}),
                )),
                Some(was) => {
                    if was.name != space.name {
                        out.push(Event::new(
                            "workspace.renamed",
                            None,
                            json!({"workspace_id": id, "label": space.name}),
                        ));
                    }
                    let at = |list: &[i64]| list.iter().position(|x| *x == space.id);
                    if at(&common) != at(&old_common) {
                        out.push(Event::new(
                            "workspace.moved",
                            None,
                            json!({"workspace_id": id, "insert_index": i,
                                   "workspaces": workspaces.clone()}),
                        ));
                    }
                }
            }
        }

        for tab in &new.tabs {
            let id = tab.id();
            let info = find(&tabs, "tab_id", &id);
            match old.tabs.iter().find(|o| o.key == tab.key) {
                None => out.push(Event::new("tab.created", None, json!({"tab": info}))),
                Some(was) => {
                    let was_info = find(&old_tabs, "tab_id", &was.id());
                    if was.workspace != tab.workspace {
                        let siblings: Vec<Value> = tabs
                            .iter()
                            .filter(|t| t["workspace_id"] == ws(tab.workspace))
                            .cloned()
                            .collect();
                        let index = siblings.iter().position(|t| t["tab_id"] == id);
                        out.push(Event::new(
                            "tab.moved",
                            None,
                            json!({"tab_id": id, "workspace_id": ws(tab.workspace),
                                   "insert_index": index, "tabs": siblings}),
                        ));
                    }
                    if was_info["label"] != info["label"] {
                        out.push(Event::new(
                            "tab.renamed",
                            None,
                            json!({"tab_id": id, "workspace_id": ws(tab.workspace),
                                   "label": info["label"]}),
                        ));
                    }
                }
            }
        }

        for row in &new.rows {
            let id = pane_id(row);
            let pane = find(&panes, "pane_id", &id);
            let agent_now = translate::agent_status(row);
            match old.rows.iter().find(|o| o.terminal == row.terminal) {
                None => {
                    out.push(Event::new(
                        "pane.created",
                        Some(id.clone()),
                        json!({"pane": pane}),
                    ));
                    if row.agent {
                        out.push(detected(&id, row, &pane));
                    }
                }
                Some(was) => {
                    if !was.agent && row.agent {
                        out.push(detected(&id, row, &pane));
                    }
                    if translate::agent_status(was) != agent_now {
                        out.push(Event::new(
                            "pane.agent_status_changed",
                            Some(id.clone()),
                            json!({"pane_id": id, "workspace_id": ws(row.workspace),
                                   "agent_status": agent_now, "agent": pane["agent"],
                                   "display_agent": pane["display_agent"],
                                   "title": pane["title"], "state_labels": {}}),
                        ));
                    }
                    let (was_key, key) = (key_of(old, was.terminal), key_of(new, row.terminal));
                    if was.workspace != row.workspace || was_key != key {
                        let previous_tab = was_key.map(|key| {
                            Id::Tab {
                                workspace: was.workspace,
                                key,
                            }
                            .to_string()
                        });
                        out.push(Event::new(
                            "pane.moved",
                            Some(id.clone()),
                            json!({"pane": pane, "previous_pane_id": pane_id(was),
                                   "previous_workspace_id": ws(was.workspace),
                                   "previous_tab_id": previous_tab}),
                        ));
                    }
                }
            }
        }

        for was in &old.rows {
            if !new.rows.iter().any(|r| r.terminal == was.terminal) {
                let data = json!({"pane_id": pane_id(was), "workspace_id": ws(was.workspace)});
                let id = Some(pane_id(was));
                out.push(Event::new("pane.closed", id.clone(), data.clone()));
                out.push(Event::new("pane.exited", id, data));
            }
        }
        for was in &old.tabs {
            if !new.tabs.iter().any(|t| t.key == was.key) {
                out.push(Event::new(
                    "tab.closed",
                    None,
                    json!({"tab_id": was.id(), "workspace_id": ws(was.workspace)}),
                ));
            }
        }
        for was in &old.spaces {
            if !new.spaces.iter().any(|s| s.id == was.id) {
                out.push(Event::new(
                    "workspace.closed",
                    None,
                    json!({"workspace_id": ws(was.id)}),
                ));
            }
        }
        out
    }
}

fn detected(id: &str, row: &AgentRow, pane: &Value) -> Event {
    Event::new(
        "pane.agent_detected",
        Some(id.to_string()),
        json!({"pane_id": id, "workspace_id": Id::Workspace(row.workspace).to_string(),
               "agent": pane["agent"]}),
    )
}

/// A herdr refusal: its code and message.
type Refused = (&'static str, String);

fn not_found(what: &'static str, id: &str) -> Refused {
    let code = match what {
        "pane" => "pane_not_found",
        "tab" => "tab_not_found",
        "agent" => "agent_not_found",
        _ => "workspace_not_found",
    };
    (code, format!("{what} {id} not found"))
}

/// The pane of `panes` that is `terminal`.
fn pane_of(panes: Vec<Value>, terminal: u32) -> Option<Value> {
    let id = terminal.to_string();
    panes
        .into_iter()
        .find(|p| p["terminal_id"].as_str() == Some(id.as_str()))
}

/// The terminal a pane id (or a bare terminal number) names.
fn terminal_of(text: &str) -> Option<u32> {
    match Id::parse(text) {
        Some(Id::Pane { terminal, .. }) => Some(terminal),
        _ => text.parse().ok().filter(|n| *n > 0),
    }
}

fn param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(Value::as_str)
}

/// The host: its view of qoop, its subscribers and its plugins.
pub(crate) struct Host {
    home: Home,
    state: Mutex<State>,
    /// Held across a listing's round trips and its update, so listings apply in order.
    refreshing: tokio::sync::Mutex<()>,
    events: broadcast::Sender<Arc<Event>>,
    procs: Mutex<Option<(Instant, Arc<Vec<ProcRow>>)>>,
    /// When the last listing was applied, so a burst of requests lists the daemon once.
    listed_at: Mutex<Option<Instant>>,
    /// How many `[[events]]` commands each plugin has running.
    running: Mutex<BTreeMap<String, Arc<AtomicUsize>>>,
}

impl Host {
    fn new(home: Home) -> Arc<Self> {
        Self::with_backlog(home, BACKLOG)
    }

    fn with_backlog(home: Home, backlog: usize) -> Arc<Self> {
        Arc::new(Self {
            home,
            state: Mutex::new(State::default()),
            refreshing: tokio::sync::Mutex::new(()),
            events: broadcast::channel(backlog).0,
            procs: Mutex::new(None),
            listed_at: Mutex::new(None),
            running: Mutex::new(BTreeMap::new()),
        })
    }

    /// List the daemon on `link` and apply it, telling every subscriber and handler.
    async fn refresh_on(&self, link: &mut Link) -> std::io::Result<()> {
        let _turn = self.refreshing.lock().await;
        let ask = ControlRequest::ListAgents {
            id: crate::request_id("plugin-host"),
        };
        let (ControlResponse::Agents { agents, splits, .. }, _) = link.ask(&ask).await? else {
            return Err(std::io::Error::other(
                "unexpected answer to the agent listing",
            ));
        };
        let ask = ControlRequest::ListWorkspaces {
            id: crate::request_id("plugin-host"),
        };
        let (ControlResponse::Workspaces { workspaces, .. }, _) = link.ask(&ask).await? else {
            return Err(std::io::Error::other(
                "unexpected answer to the space listing",
            ));
        };
        let events = self
            .state
            .lock()
            .unwrap()
            .update(workspaces, agents, &splits);
        for event in events {
            let _ = self.events.send(Arc::new(event));
        }
        *self.listed_at.lock().unwrap() = Some(Instant::now());
        Ok(())
    }

    /// A fresh listing on a connection of its own, for a request about to read the state. One
    /// applied in the last [`FRESH`] is fresh enough; with no daemon to ask (a live upgrade
    /// between its two halves), the last listing answers.
    async fn refresh(&self) -> Result<(), Refused> {
        if self
            .listed_at
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() < FRESH)
        {
            return Ok(());
        }
        let primed = || self.state.lock().unwrap().primed;
        let unavailable = |e: std::io::Error| ("server_unavailable", e.to_string());
        match Link::open_observer("qoop-plugin-host").await {
            Ok(mut link) => match self.refresh_on(&mut link).await {
                Err(_) if primed() => Ok(()),
                other => other.map_err(unavailable),
            },
            Err(_) if primed() => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }

    /// Every process this user can read, reused for half a second.
    fn procs(&self) -> Arc<Vec<ProcRow>> {
        let mut held = self.procs.lock().unwrap();
        if let Some((at, table)) = held.as_ref()
            && at.elapsed() < PROCS_FRESH
        {
            return table.clone();
        }
        let table = Arc::new(qoop_pty::sys::process_table());
        *held = Some((Instant::now(), table.clone()));
        table
    }

    /// Where a terminal is now: its foreground group leader's directory (else any member's),
    /// else its shell's, else what the row last said.
    fn live_cwd(&self, row: &AgentRow) -> Option<String> {
        let procs = self.procs();
        let foreground = row.pgid.and_then(|group| {
            procs
                .iter()
                .find(|p| p.pid == group)
                .or_else(|| procs.iter().find(|p| p.pgid == group))
                .map(|p| p.pid)
        });
        let shell = row.shell_pid.and_then(|pid| i32::try_from(pid).ok());
        foreground
            .into_iter()
            .chain(shell)
            .find_map(|pid| qoop_pty::sys::cwd_of(qoop_pty::Pid(pid)))
            .map(|path| path.display().to_string())
            .or_else(|| row.cwd.clone())
    }

    /// Read a copy of the state with each row's directory made live; the lock is not held
    /// while the process table is read.
    fn with_view<R>(&self, read: impl FnOnce(&View) -> R) -> R {
        let (mut listing, revisions, focused) = {
            let state = self.state.lock().unwrap();
            (state.now.clone(), state.revisions.clone(), state.focused)
        };
        for row in &mut listing.rows {
            row.cwd = self.live_cwd(row);
        }
        read(&View {
            spaces: &listing.spaces,
            rows: &listing.rows,
            tabs: &listing.tabs,
            focused,
            revisions: &revisions,
        })
    }

    /// One request, answered in herdr's envelope.
    async fn answer(&self, id: &str, method: &str, params: &Value) -> Value {
        if !translate::METHODS.contains(&method) {
            return translate::unknown_method(id, method);
        }
        match self.result(method, params).await {
            Ok(result) => translate::reply(id, result),
            Err((code, message)) => translate::refuse(id, code, &message),
        }
    }

    async fn result(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        if !SERVED.contains(&method) {
            return Err(("not_supported", format!("{method} is not served yet")));
        }
        match method {
            "ping" => {
                return Ok(json!({"type": "pong", "version": translate::VERSION,
                                 "protocol": translate::PROTOCOL}));
            }
            "server.reload_config" => {
                return Ok(
                    json!({"type": "config_reload", "status": "applied", "diagnostics": []}),
                );
            }
            "pane.report_metadata" | "workspace.report_metadata" => {
                return Ok(json!({"type": "ok"}));
            }
            "plugin.list" => {
                return Ok(json!({"type": "plugin_list",
                                 "plugins": plugins(&self.home, param(params, "plugin_id"))}));
            }
            "plugin.action.list" => {
                return Ok(json!({"type": "plugin_action_list",
                                 "actions": actions(&self.home, param(params, "plugin_id"))}));
            }
            _ => {}
        }
        self.refresh().await?;
        match method {
            "pane.read" => self.read(params).await,
            "pane.process_info" => self.process_info(params),
            _ => self.with_view(|view| answer_from(view, method, params)),
        }
    }

    async fn read(&self, params: &Value) -> Result<Value, Refused> {
        let asked = param(params, "pane_id").unwrap_or_default();
        let terminal = terminal_of(asked).ok_or_else(|| not_found("pane", asked))?;
        let pane = self
            .with_view(|view| pane_of(view.panes(), terminal))
            .ok_or_else(|| not_found("pane", asked))?;
        let source = param(params, "source").unwrap_or("visible").to_string();
        let lines = params
            .get("lines")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let unavailable = |e: std::io::Error| ("server_unavailable", e.to_string());
        let rows: Vec<String> = if source == "visible" {
            let snapshot = crate::fetch_snapshot(terminal, "qoop-plugin-host")
                .await
                .map_err(unavailable)?;
            let mut rows = snapshot.lines;
            while rows.last().is_some_and(|r| r.trim().is_empty()) {
                rows.pop();
            }
            let keep = lines.unwrap_or(rows.len()).min(rows.len());
            rows.split_off(rows.len() - keep)
        } else {
            let n = lines.unwrap_or(200);
            let mut link = Link::open_observer("qoop-plugin-host")
                .await
                .map_err(unavailable)?;
            let ask = ControlRequest::Scrollback {
                id: crate::request_id("plugin-host"),
                terminal,
                up: n,
                rows: n,
            };
            match link.ask(&ask).await.map_err(unavailable)?.0 {
                ControlResponse::Scrollback { lines, .. } => lines,
                ControlResponse::Failed { reason, .. } => return Err(("read_failed", reason)),
                _ => return Err(("read_failed", "unexpected answer to a history read".into())),
            }
        };
        Ok(json!({"type": "pane_read", "read": {
            "pane_id": pane["pane_id"], "workspace_id": pane["workspace_id"],
            "tab_id": pane["tab_id"], "source": source,
            "format": param(params, "format").unwrap_or("text"),
            "text": rows.join("\n"), "revision": pane["revision"], "truncated": false,
        }}))
    }

    fn process_info(&self, params: &Value) -> Result<Value, Refused> {
        let asked = param(params, "pane_id").unwrap_or_default();
        let row = {
            let state = self.state.lock().unwrap();
            let terminal = terminal_of(asked).or(state.focused);
            state
                .now
                .rows
                .iter()
                .find(|r| Some(r.terminal) == terminal)
                .cloned()
        }
        .ok_or_else(|| not_found("pane", asked))?;
        let procs = self.procs();
        let foreground: Vec<translate::Process> = procs
            .iter()
            .filter(|p| Some(p.pgid) == row.pgid)
            .map(|p| translate::Process {
                pid: p.pid,
                name: p.name.clone(),
                argv: qoop_pty::sys::argv_of(qoop_pty::Pid(p.pid)),
                cwd: qoop_pty::sys::cwd_of(qoop_pty::Pid(p.pid)).map(|c| c.display().to_string()),
            })
            .collect();
        Ok(json!({"type": "pane_process_info",
                  "process_info": translate::process_info(&row, &foreground)}))
    }
}

/// The read methods answered from the state alone.
fn answer_from(view: &View, method: &str, params: &Value) -> Result<Value, Refused> {
    let only = |list: Vec<Value>, key: &str| match param(params, key) {
        Some(id) => list.into_iter().filter(|v| v[key] == id).collect(),
        None => list,
    };
    let one = |list: Vec<Value>, key: &str, what: &'static str| {
        let id = param(params, key).unwrap_or_default();
        list.into_iter()
            .find(|v| v[key] == id)
            .ok_or_else(|| not_found(what, id))
    };
    match method {
        "session.snapshot" => {
            Ok(json!({"type": "session_snapshot", "snapshot": view.snapshot(AREA)}))
        }
        "pane.list" => {
            Ok(json!({"type": "pane_list", "panes": only(view.panes(), "workspace_id")}))
        }
        "pane.get" => {
            let asked = param(params, "pane_id").unwrap_or_default();
            let pane = terminal_of(asked)
                .and_then(|t| pane_of(view.panes(), t))
                .ok_or_else(|| not_found("pane", asked))?;
            Ok(json!({"type": "pane_info", "pane": pane}))
        }
        "pane.current" => {
            let terminal = view
                .focused
                .or_else(|| param(params, "caller_pane_id").and_then(terminal_of))
                .ok_or(("pane_not_found", "no focused pane is known".to_string()))?;
            let pane = pane_of(view.panes(), terminal)
                .ok_or_else(|| not_found("pane", &terminal.to_string()))?;
            Ok(json!({"type": "pane_current", "pane": pane}))
        }
        "pane.layout" | "pane.resize" => {
            let asked = param(params, "pane_id");
            let terminal = asked.and_then(terminal_of).or(view.focused);
            let tab = view
                .tabs
                .iter()
                .find(|t| terminal.is_some_and(|x| t.leaves.contains(&x)))
                .ok_or_else(|| not_found("pane", asked.unwrap_or("current")))?;
            let layout = translate::layout(tab, AREA, view.focused, false);
            if method == "pane.layout" {
                return Ok(json!({"type": "pane_layout", "layout": layout}));
            }
            // qoop keeps no pane sizes to change: herdr's answer for a resize that moved nothing.
            Ok(json!({"type": "pane_resize", "resize": {
                "changed": false,
                "pane_id": Id::Pane { workspace: tab.workspace, terminal: terminal.unwrap_or(tab.leaves[0]) }.to_string(),
                "focused_pane_id": layout["focused_pane_id"].clone(), "layout": layout,
            }}))
        }
        "tab.list" => {
            Ok(json!({"type": "tab_list", "tabs": only(view.tab_infos(), "workspace_id")}))
        }
        "tab.get" => {
            Ok(json!({"type": "tab_info", "tab": one(view.tab_infos(), "tab_id", "tab")?}))
        }
        "workspace.list" => {
            Ok(json!({"type": "workspace_list", "workspaces": view.workspace_infos()}))
        }
        "workspace.get" => Ok(json!({"type": "workspace_info",
            "workspace": one(view.workspace_infos(), "workspace_id", "workspace")?})),
        "agent.list" => Ok(json!({"type": "agent_list", "agents": view.agents()})),
        "agent.get" => {
            let target = param(params, "target").unwrap_or_default();
            let terminal = terminal_of(target).map(|t| t.to_string());
            let agent = view
                .agents()
                .into_iter()
                .find(|a| {
                    Some(a["terminal_id"].as_str().unwrap_or_default().to_string()) == terminal
                        || a["name"] == target
                })
                .ok_or_else(|| not_found("agent", target))?;
            Ok(json!({"type": "agent_info", "agent": agent}))
        }
        _ => Err(("not_supported", format!("{method} is not served"))),
    }
}

/// herdr's `InstalledPluginInfo` for each plugin here (or the one asked for).
fn plugins(home: &Home, only: Option<&str>) -> Vec<Value> {
    install::installed(home)
        .into_iter()
        .filter(|id| only.is_none_or(|o| o == id))
        .map(|id| {
            let checked = install::admitted(home, &id);
            let root = home
                .dir_of(&id)
                .canonicalize()
                .unwrap_or_else(|_| home.dir_of(&id));
            let (version, source) = match &checked {
                Ok(a) => (a.manifest.version.clone(), source_info(a)),
                Err(_) => (None, json!({"kind": "local"})),
            };
            json!({
                "plugin_id": id, "name": id, "version": version.unwrap_or_default(),
                "manifest_path": root.join(manifest::FILE), "plugin_root": root,
                "enabled": checked.is_ok(), "source": source,
            })
        })
        .collect()
}

/// herdr's `PluginSourceInfo` from what was agreed to.
fn source_info(plugin: &Admitted) -> Value {
    let Some(sha) = &plugin.consent.sha else {
        return json!({"kind": "local", "managed_path": null});
    };
    let mut parts = plugin.consent.source.splitn(3, '/');
    let (owner, repo, subdir) = (parts.next(), parts.next(), parts.next());
    json!({"kind": "github", "managed_path": plugin.root, "owner": owner, "repo": repo,
           "subdir": subdir, "resolved_commit": sha})
}

/// herdr's `PluginActionInfo` for every action this machine runs.
fn actions(home: &Home, only: Option<&str>) -> Vec<Value> {
    install::installed(home)
        .into_iter()
        .filter(|id| only.is_none_or(|o| o == id))
        .filter_map(|id| install::admitted(home, &id).ok().map(|a| (id, a)))
        .flat_map(|(id, a)| {
            a.manifest.here().actions.into_iter().map(move |action| {
                json!({"plugin_id": id, "action_id": action.id,
                           "title": action.title.unwrap_or_else(|| action.id.clone()),
                           "command": action.command, "contexts": action.contexts})
            })
        })
        .collect()
}

/// The `[[events]]` of a manifest that run for a hook name.
fn hooks_for<'a>(hooks: &'a [Hook], name: &str) -> Vec<&'a Hook> {
    hooks
        .iter()
        .filter(|h| translate::written(translate::HOOK_EVENTS, &h.on) == Some(name))
        .collect()
}

/// What a handler is told it is about: `HERDR_PLUGIN_CONTEXT_JSON` and the matching
/// `HERDR_WORKSPACE_ID`, `_TAB_ID`, `_PANE_ID` and `_WORKSPACE_CWD`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Scope {
    pub(crate) context: Value,
    pub(crate) env: Vec<(String, OsString)>,
}

/// The id an event's data names under `key`, directly or inside its `object`.
fn named<'a>(data: &'a Value, key: &str, object: &str) -> Option<&'a str> {
    data.get(key)
        .or_else(|| data.get(object).and_then(|o| o.get(key)))
        .and_then(Value::as_str)
}

/// The scope of an event: its pane, else its tab, else its space, found in the listing it
/// was made from (the previous one for what has just closed).
fn scope_of(event: &Event, new: &Listing, old: &Listing) -> Scope {
    let data = &event.data;
    // The narrowest thing named wins, in whichever listing still has it: a closed pane's own
    // pane and tab, never just the space it was in.
    let terminal = named(data, "pane_id", "pane").and_then(terminal_of);
    for listing in [new, old] {
        if let Some(row) = terminal.and_then(|t| listing.rows.iter().find(|r| r.terminal == t)) {
            return scope(listing, row.workspace, Some(row.terminal));
        }
    }
    let tab_id = named(data, "tab_id", "tab");
    for listing in [new, old] {
        if let Some(tab) = tab_id.and_then(|id| listing.tabs.iter().find(|t| t.id() == id)) {
            return scope(listing, tab.workspace, Some(tab.leaves[0]));
        }
    }
    let space = named(data, "workspace_id", "workspace").and_then(Id::parse);
    for listing in [new, old] {
        if let Some(Id::Workspace(space)) = space
            && listing.spaces.iter().any(|s| s.id == space)
        {
            return scope(listing, space, None);
        }
    }
    Scope {
        context: json!({"selected_text": ""}),
        env: Vec::new(),
    }
}

/// The scope of a space and, when given, one terminal in it.
fn scope(listing: &Listing, workspace: i64, terminal: Option<u32>) -> Scope {
    let space = listing.spaces.iter().find(|s| s.id == workspace);
    let row = terminal.and_then(|t| listing.rows.iter().find(|r| r.terminal == t));
    let tab = terminal.and_then(|t| listing.tabs.iter().find(|tab| tab.leaves.contains(&t)));
    let tab_label = tab
        .and_then(|tab| listing.rows.iter().find(|r| r.terminal == tab.leaves[0]))
        .and_then(|r| r.name.clone().or_else(|| r.program.clone()));
    let workspace_id = Id::Workspace(workspace).to_string();
    let workspace_cwd = space
        .and_then(|s| s.dir.clone())
        .or_else(|| row.and_then(|r| r.cwd.clone()));
    let pane_id = row.map(|r| {
        Id::Pane {
            workspace: r.workspace,
            terminal: r.terminal,
        }
        .to_string()
    });
    let context = json!({
        "workspace_id": workspace_id,
        "workspace_label": space.map(|s| s.name.clone()),
        "workspace_cwd": workspace_cwd,
        "tab_id": tab.map(Tab::id),
        "tab_label": tab_label,
        "focused_pane_id": pane_id,
        "focused_pane_cwd": row.and_then(|r| r.cwd.clone()),
        "focused_pane_agent": row.and_then(|r| r.agent.then(|| r.program.clone()).flatten()),
        "selected_text": "",
    });
    let mut env: Vec<(String, OsString)> = vec![("HERDR_WORKSPACE_ID".into(), workspace_id.into())];
    if let Some(tab) = tab {
        env.push(("HERDR_TAB_ID".into(), tab.id().into()));
    }
    if let Some(pane) = pane_id {
        env.push(("HERDR_PANE_ID".into(), pane.into()));
    }
    if let Some(cwd) = workspace_cwd {
        env.push(("HERDR_WORKSPACE_CWD".into(), cwd.into()));
    }
    Scope { context, env }
}

/// The scope of what the interface shows, when it has said; for a startup command.
fn focused_scope(state: &State) -> Scope {
    match state
        .focused
        .and_then(|t| state.now.rows.iter().find(|r| r.terminal == t))
    {
        Some(row) => scope(&state.now, row.workspace, Some(row.terminal)),
        None => Scope {
            context: json!({"selected_text": ""}),
            env: Vec::new(),
        },
    }
}

/// A plugin's host log, moved aside to `host.log.1` once it passes [`LOG_CAP`].
fn open_log(log: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(log).is_ok_and(|m| m.len() > LOG_CAP) {
        let _ = std::fs::rename(log, log.with_extension("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
}

/// One line in a plugin's host log; a log that cannot be written is not worth failing over,
/// and a plugin that has been removed gets none (its state folder is not made again).
fn note(home: &Home, id: &str, line: &str) {
    if std::fs::symlink_metadata(home.dir_of(id)).is_err() {
        return;
    }
    if let Ok(mut file) = open_log(&home.state_of(id).join("host.log")) {
        let _ = writeln!(file, "qoop plugin host: {line}");
    }
}

/// Run one plugin command in its root with its environment, in a process group of its own,
/// output appended to `log`. The caller bounds it ([`wait_bounded`]) or records its group.
fn spawn(
    root: &Path,
    argv: &[String],
    env: Vec<(String, OsString)>,
    log: &Path,
) -> std::io::Result<std::process::Child> {
    let out = open_log(log)?;
    let mut cmd = std::process::Command::new(install::program(root, &argv[0]));
    cmd.args(&argv[1..])
        .current_dir(root)
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0);
    install::scrub(&mut cmd);
    cmd.spawn()
}

/// Wait for a handler, killing its whole process group at `limit`. `false` when it had to.
fn wait_bounded(mut child: std::process::Child, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        if let Ok(Some(_)) | Err(_) = child.try_wait() {
            return true;
        }
        if Instant::now() >= until {
            // SAFETY: killpg on the group this child leads; it was spawned with
            // `process_group(0)`, so the group id is its pid.
            unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The startup commands running for each plugin, kept in `.state/.host-pgids` as
/// `<plugin> <group> <start time>` lines: written when one starts, struck when it ends, and
/// read by a host that starts after one that was killed, and by `rm`.
fn pgids_file(home: &Home) -> PathBuf {
    home.state_of(".host-pgids")
}

fn read_pgids(file: &Path) -> Vec<(String, i32, u64)> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut words = line.split(' ');
            let id = words.next()?.to_string();
            let group = words.next()?.parse().ok()?;
            let started = words.next()?.parse().ok()?;
            Some((id, group, started))
        })
        .collect()
}

/// Change the startup list under its lock.
fn edit_pgids(home: &Home, change: impl FnOnce(&mut Vec<(String, i32, u64)>)) {
    let file = pgids_file(home);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(_held) = qoop_runtime::lock::StartupLock::acquire(&file.with_extension("lock")) else {
        return;
    };
    let mut groups = read_pgids(&file);
    change(&mut groups);
    let text: String = groups
        .iter()
        .map(|(id, group, started)| format!("{id} {group} {started}\n"))
        .collect();
    let _ = std::fs::write(&file, text);
}

/// End the recorded startup commands of `only` (every plugin when `None`): each group whose
/// leader is still the process that was started, by its start time, so a reused pid is left
/// alone.
pub(crate) fn end_startups(home: &Home, only: Option<&str>) {
    edit_pgids(home, |groups| {
        groups.retain(|(id, group, started)| {
            if only.is_some_and(|o| o != id) {
                return true;
            }
            if qoop_pty::sys::start_time_of(qoop_pty::Pid(*group)) == Some(*started) {
                // SAFETY: killpg on a group this host started with `process_group(0)`,
                // checked to still be led by that process.
                unsafe { libc::killpg(*group, libc::SIGTERM) };
            }
            false
        });
    });
}

impl Host {
    /// Run every plugin's `[[events]]` for one event, each bounded by [`HANDLER_LIMIT`] and at
    /// most [`HANDLERS_PER_PLUGIN`] at once per plugin; one past that is dropped and logged.
    fn run_hooks(&self, event: &Event) {
        let Some((name, payload)) = event.for_hook() else {
            return;
        };
        for id in install::installed(&self.home) {
            let log = self.home.state_of(&id).join("host.log");
            let plugin = match install::admitted(&self.home, &id) {
                Ok(plugin) => plugin,
                Err(refused) => {
                    if !hooks_for_unchecked(&self.home, &id, name) {
                        continue;
                    }
                    note(&self.home, &id, &format!("{name}: not run: {refused}"));
                    continue;
                }
            };
            for hook in hooks_for(&plugin.manifest.here().events, name) {
                let running = self
                    .running
                    .lock()
                    .unwrap()
                    .entry(id.clone())
                    .or_default()
                    .clone();
                if running.fetch_add(1, Ordering::SeqCst) >= HANDLERS_PER_PLUGIN {
                    running.fetch_sub(1, Ordering::SeqCst);
                    note(
                        &self.home,
                        &id,
                        &format!("{name}: dropped, {HANDLERS_PER_PLUGIN} already running"),
                    );
                    continue;
                }
                let mut env = install::plugin_env(&self.home, &id, &plugin.root);
                env.extend(event.scope.env.clone());
                env.push(("HERDR_PLUGIN_EVENT".into(), name.into()));
                env.push(("HERDR_PLUGIN_EVENT_JSON".into(), payload.to_string().into()));
                env.push((
                    "HERDR_PLUGIN_CONTEXT_JSON".into(),
                    event.scope.context.to_string().into(),
                ));
                match spawn(&plugin.root, &hook.command, env, &log) {
                    Ok(child) => {
                        std::thread::spawn(move || {
                            wait_bounded(child, HANDLER_LIMIT);
                            running.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    Err(e) => {
                        running.fetch_sub(1, Ordering::SeqCst);
                        note(&self.home, &id, &format!("{name}: {:?}: {e}", hook.command));
                    }
                }
            }
        }
    }

    /// Start every plugin's `[[startup]]` commands, long-lived, once per host; the ones a
    /// killed host left are ended first, so none runs twice.
    fn run_startups(&self) {
        end_startups(&self.home, None);
        let scope = focused_scope(&self.state.lock().unwrap());
        for id in install::installed(&self.home) {
            let log = self.home.state_of(&id).join("host.log");
            let plugin = match install::admitted(&self.home, &id) {
                Ok(plugin) => plugin,
                Err(refused) => {
                    note(&self.home, &id, &format!("startup: not run: {refused}"));
                    continue;
                }
            };
            for step in plugin.manifest.here().startup {
                let mut env = install::plugin_env(&self.home, &id, &plugin.root);
                env.extend(scope.env.clone());
                env.push((
                    "HERDR_PLUGIN_CONTEXT_JSON".into(),
                    scope.context.to_string().into(),
                ));
                match spawn(&plugin.root, &step.command, env, &log) {
                    Ok(mut child) => {
                        let group = child.id() as i32;
                        let started = qoop_pty::sys::start_time_of(qoop_pty::Pid(group));
                        let home = self.home.clone();
                        if let Some(started) = started {
                            let id = id.clone();
                            edit_pgids(&home, |groups| groups.push((id, group, started)));
                        }
                        std::thread::spawn(move || {
                            let _ = child.wait();
                            edit_pgids(&home, |groups| groups.retain(|(_, g, _)| *g != group));
                        });
                    }
                    Err(e) => note(&self.home, &id, &format!("startup {:?}: {e}", step.command)),
                }
            }
        }
    }
}

/// Whether a plugin that is no longer agreed to has a hook for `name`, read without running
/// anything, so its log says why the hook did not run instead of every event being logged.
fn hooks_for_unchecked(home: &Home, id: &str, name: &str) -> bool {
    std::fs::read_to_string(home.dir_of(id).join(manifest::FILE))
        .ok()
        .and_then(|text| manifest::Manifest::parse(&text).ok())
        .is_some_and(|m| !hooks_for(&m.events, name).is_empty())
}

/// Whether events say something that changes what the daemon lists: a terminal's state or
/// the whole list. A match over every kind, so a new one is decided here, not missed.
fn moves_rows(event: &EventMessage) -> bool {
    match event {
        EventMessage::AgentChanged { .. } | EventMessage::Resync => true,
        EventMessage::Focus { .. }
        | EventMessage::TasksChanged
        | EventMessage::AsksChanged
        | EventMessage::MessagesChanged { .. }
        | EventMessage::ShowsChanged
        | EventMessage::SchedulesChanged
        | EventMessage::Listen { .. } => false,
    }
}

/// Wait on the link until the rows may have moved or the poll is due. A burst of changes is
/// let settle for [`COALESCE`] and read as one. `Err` when the daemon closed the link.
async fn changed(link: &mut Link) -> std::io::Result<()> {
    let poll = Instant::now() + POLL;
    loop {
        match link.next_events(Some(poll)).await? {
            None => return Ok(()),
            Some(events) if events.iter().any(moves_rows) => {
                tokio::time::sleep(COALESCE).await;
                // What came meanwhile is read with it, for one more window at most, so a daemon
                // that never goes quiet still gets listed.
                let until = Instant::now() + COALESCE;
                let soon = || Some((Instant::now() + Duration::from_millis(1)).min(until));
                while Instant::now() < until && link.next_events(soon()).await?.is_some() {}
                return Ok(());
            }
            Some(_) => {}
        }
    }
}

/// Follow the daemon: list it now, then on every change and every [`POLL`]. Returns when it
/// has been gone for [`RECONNECT`], or when no plugin is left to serve.
async fn follow(host: &Host) {
    let mut gone_since: Option<Instant> = None;
    loop {
        if install::installed(&host.home).is_empty() {
            return;
        }
        if let Ok(mut link) = Link::open_observer("qoop-plugin-host").await {
            loop {
                if host.refresh_on(&mut link).await.is_err() {
                    break;
                }
                gone_since = None;
                if install::installed(&host.home).is_empty() || changed(&mut link).await.is_err() {
                    break;
                }
            }
        }
        if gone_since.get_or_insert_with(Instant::now).elapsed() > RECONNECT {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The id a request carries, echoed as it came. A string, as herdr takes it: herdr refuses a
/// number too.
fn request_id(request: &Value) -> Option<Value> {
    match &request["id"] {
        id @ Value::String(_) => Some(id.clone()),
        _ => None,
    }
}

/// A line answered under `id` as the request wrote it.
fn under(mut line: Value, id: &Value) -> Value {
    line["id"] = id.clone();
    line
}

/// Serve one connection: requests answered in order, until it subscribes, after which it is
/// an event stream.
async fn serve(host: Arc<Host>, stream: tokio::net::UnixStream) {
    // ADR-012: only this user, whatever the socket's mode says.
    // SAFETY: getuid cannot fail and reads nothing of ours.
    let me = unsafe { libc::getuid() };
    if qoop_runtime::daemon::peer_uid(&stream).ok() != Some(me) {
        return;
    }
    let (rd, mut wr) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(rd).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let parsed: Result<Value, String> = serde_json::from_str(&line).map_err(|e| e.to_string());
        let (request, id) = match parsed.and_then(|r| {
            let id = request_id(&r).ok_or("missing or invalid field `id`".to_string())?;
            Ok((r, id))
        }) {
            Ok(found) => found,
            Err(e) => {
                let refused =
                    translate::refuse("", "invalid_request", &format!("invalid request: {e}"));
                if send(&mut wr, &under(refused, &json!(""))).await.is_err() {
                    return;
                }
                continue;
            }
        };
        let method = request["method"].as_str().unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(json!({}));
        if method == "events.subscribe" {
            stream_events(&host, &id, &params, wr).await;
            return;
        }
        let answer = host.answer("", method, &params).await;
        if send(&mut wr, &under(answer, &id)).await.is_err() {
            return;
        }
    }
}

async fn send(wr: &mut tokio::net::unix::OwnedWriteHalf, line: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(line).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    wr.write_all(&bytes).await?;
    wr.flush().await
}

/// The subscription types herdr binds to one pane, which must name it.
const PER_PANE: &[&str] = &[
    "pane.output_matched",
    "pane.agent_status_changed",
    "pane.scroll_changed",
];

/// What a subscription asks for: dotted types, each optionally bound to a pane.
fn subscriptions(params: &Value) -> Result<Vec<(String, Option<String>)>, String> {
    let list = params["subscriptions"]
        .as_array()
        .ok_or("invalid request: missing field `subscriptions`")?;
    list.iter()
        .map(|s| {
            let kind = s["type"].as_str().unwrap_or_default();
            if translate::written(translate::SUBSCRIPTIONS, kind).is_none() {
                return Err(format!("invalid request: unknown variant `{kind}`"));
            }
            let pane = s["pane_id"].as_str().map(str::to_string);
            if PER_PANE.contains(&kind) && pane.is_none() {
                return Err(format!("invalid request: {kind} needs a `pane_id`"));
            }
            Ok((kind.to_string(), pane))
        })
        .collect()
}

async fn stream_events(
    host: &Host,
    id: &Value,
    params: &Value,
    mut wr: tokio::net::unix::OwnedWriteHalf,
) {
    let wanted = match subscriptions(params) {
        Ok(wanted) => wanted,
        Err(message) => {
            let refused = translate::refuse("", "invalid_request", &message);
            let _ = send(&mut wr, &under(refused, id)).await;
            return;
        }
    };
    let panes: Vec<&String> = wanted.iter().filter_map(|(_, p)| p.as_ref()).collect();
    if !panes.is_empty() {
        let _ = host.refresh().await;
        let known = |p: &String| {
            terminal_of(p).is_some_and(|t| {
                host.state
                    .lock()
                    .unwrap()
                    .now
                    .rows
                    .iter()
                    .any(|r| r.terminal == t)
            })
        };
        if let Some(missing) = panes.into_iter().find(|p| !known(p)) {
            let refused =
                translate::refuse("", "pane_not_found", &format!("pane {missing} not found"));
            let _ = send(&mut wr, &under(refused, id)).await;
            return;
        }
    }
    let mut rx = host.events.subscribe();
    let ack = translate::reply("", json!({"type": "subscription_started"}));
    if send(&mut wr, &under(ack, id)).await.is_err() {
        return;
    }
    loop {
        let line = match rx.recv().await {
            Ok(event) => {
                let hit = wanted.iter().any(|(kind, pane)| {
                    kind == event.dotted
                        && pane.as_ref().is_none_or(|p| Some(p) == event.pane.as_ref())
                });
                match event.for_subscriber() {
                    Some(line) if hit => line,
                    _ => continue,
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => under(
                translate::refuse(
                    "",
                    "events_lost",
                    &format!("{n} events were dropped; read a fresh snapshot"),
                ),
                id,
            ),
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if send(&mut wr, &line).await.is_err() {
            return;
        }
    }
}

/// `qoop plugin host`.
pub(crate) fn run() -> std::io::Result<()> {
    let home = Home::here()?;
    if install::installed(&home).is_empty() {
        return Ok(());
    }
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    // One host per session: a second one started in a race leaves quietly.
    let Some(_held) = qoop_runtime::lock::StartupLock::try_acquire(&dir.join("herdr.lock"))? else {
        return Ok(());
    };
    super::shim::place(&home)?;
    let socket = dir.join(SOCKET);
    tokio::runtime::Runtime::new()?.block_on(async {
        qoop_runtime::daemon::remove_stale_socket(&socket)?;
        let listener = tokio::net::UnixListener::bind(&socket)?;
        // Restricted right after the bind; until then the verified 0700 directory around it
        // is what keeps others out, as for the daemon's own socket.
        std::fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let host = Host::new(home);
        let hooks = host.clone();
        let mut heard = host.events.subscribe();
        tokio::spawn(async move {
            loop {
                match heard.recv().await {
                    Ok(event) => hooks.run_hooks(&event),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        let serving = host.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(serving.clone(), stream));
            }
        });
        let _ = host.refresh().await;
        host.run_startups();
        follow(&host).await;
        end_startups(&host.home, None);
        let _ = std::fs::remove_file(&socket);
        Ok(())
    })
}

/// Whether a host needs starting: a plugin is installed and nothing answers on the socket.
fn host_wanted(home: &Home, socket: &Path) -> bool {
    !install::installed(home).is_empty() && std::os::unix::net::UnixStream::connect(socket).is_err()
}

/// Start the host when a plugin is installed and none answers yet. Detached, with qoop's own
/// variables taken out of what it inherits except the ones that say which daemon to follow.
pub(crate) fn ensure_plugin_host() {
    let Ok(home) = Home::here() else {
        return;
    };
    if install::installed(&home).is_empty() {
        return;
    }
    let Ok(dir) = paths::runtime_dir() else {
        return;
    };
    if !host_wanted(&home, &dir.join(SOCKET)) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["plugin", "host"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    // The runtime and session say which daemon this is; a terminal's token and name never
    // travel to it.
    const KEEP: &[&str] = &[
        "QOOP_RUNTIME_DIR",
        "QORO_RUNTIME_DIR",
        "QOOP_SESSION",
        "QORO_SESSION",
    ];
    for (name, _) in std::env::vars_os() {
        let bytes = name.as_encoded_bytes();
        let ours = bytes.starts_with(b"QOOP_") || bytes.starts_with(b"QORO_");
        if ours && !KEEP.iter().any(|k| name == *k) {
            cmd.env_remove(name);
        }
    }
    qoop_runtime::daemon::scrub_parent_agent_env(&mut cmd);
    if let Ok(mut child) = cmd.spawn() {
        // Reaped when it leaves, so a long-lived caller (the interface) keeps no zombie.
        std::thread::spawn(move || child.wait());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(terminal: u32, workspace: i64, extra: Value) -> AgentRow {
        let mut base = json!({"terminal": terminal, "workspace": workspace, "state": "idle"});
        if let (Value::Object(base), Value::Object(extra)) = (&mut base, extra) {
            base.extend(extra);
        }
        serde_json::from_value(base).expect("a row")
    }

    fn space(id: i64, name: &str) -> WorkspaceRow {
        serde_json::from_value(json!({"id": id, "name": name, "terminals": 0})).expect("a space")
    }

    fn split(first: u32, second: u32) -> Pane {
        Pane::Split {
            dir: qoop_runtime::control::SplitDir::V,
            permille: 500,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: first }),
            second: Box::new(Pane::Leaf { terminal: second }),
        }
    }

    fn names(events: &[Event]) -> Vec<&'static str> {
        events.iter().map(|e| e.dotted).collect()
    }

    fn primed() -> State {
        let mut state = State::default();
        let first = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        assert!(first.is_empty(), "the first listing is the starting point");
        state
    }

    #[test]
    fn a_new_pane_and_tab_are_created_and_a_closed_one_closes() {
        let mut state = primed();
        let events = state.update(
            vec![space(1, "api")],
            vec![
                row(1, 1, json!({})),
                row(2, 1, json!({"agent": true, "program": "claude"})),
            ],
            &[],
        );
        assert_eq!(
            names(&events),
            ["tab.created", "pane.created", "pane.agent_detected"]
        );
        assert_eq!(events[1].data["pane"]["pane_id"], "w1:p2");
        assert_eq!(events[0].data["tab"]["tab_id"], "w1:t2");
        let (hook, payload) = events[1].for_hook().expect("a hook name");
        assert_eq!(hook, "pane_created");
        assert_eq!(payload["data"]["type"], "pane_created");

        let events = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        assert_eq!(names(&events), ["pane.closed", "pane.exited", "tab.closed"]);
        assert_eq!(
            events[0].data,
            json!({"pane_id": "w1:p2", "workspace_id": "w1"})
        );
        assert_eq!(events[2].data["tab_id"], "w1:t2");
    }

    #[test]
    fn a_status_change_is_one_per_pane_event_spelt_dotted() {
        let mut state = State::default();
        let agent = |s: &str| row(3, 1, json!({"agent": true, "program": "codex", "state": s}));
        state.update(vec![space(1, "a")], vec![agent("working")], &[]);
        let events = state.update(vec![space(1, "a")], vec![agent("blocked")], &[]);
        assert_eq!(names(&events), ["pane.agent_status_changed"]);
        assert_eq!(events[0].pane.as_deref(), Some("w1:p3"));
        let line = events[0].for_subscriber().expect("subscribed name");
        assert_eq!(line["event"], "pane.agent_status_changed");
        assert_eq!(line["data"]["agent_status"], "blocked");
        assert!(
            line["data"].get("type").is_none(),
            "per-pane events carry no type"
        );
        let (hook, payload) = events[0].for_hook().expect("hook");
        assert_eq!(hook, "pane_agent_status_changed");
        assert_eq!(payload["data"]["type"], "pane_agent_status_changed");
    }

    #[test]
    fn renames_and_moves_are_told() {
        let mut state = State::default();
        let spaces = vec![space(1, "a"), space(2, "b")];
        state.update(
            spaces.clone(),
            vec![row(4, 1, json!({"name": "logs"}))],
            &[],
        );
        let events = state.update(
            vec![space(1, "a"), space(2, "b")],
            vec![row(4, 1, json!({"name": "tail"}))],
            &[],
        );
        assert_eq!(names(&events), ["tab.renamed"]);
        assert_eq!(events[0].data["label"], "tail");

        let events = state.update(
            vec![space(2, "b"), space(1, "a2")],
            vec![row(4, 2, json!({"name": "tail"}))],
            &[],
        );
        let got = names(&events);
        for want in [
            "workspace.renamed",
            "workspace.moved",
            "tab.moved",
            "pane.moved",
        ] {
            assert!(got.contains(&want), "{want} missing from {got:?}");
        }
        let moved = events
            .iter()
            .find(|e| e.dotted == "pane.moved")
            .expect("moved");
        assert_eq!(moved.data["previous_workspace_id"], "w1");
        assert_eq!(moved.data["pane"]["workspace_id"], "w2");
    }

    #[test]
    fn a_tab_keeps_its_id_when_its_first_leaf_closes() {
        let mut state = State::default();
        state.update(
            vec![space(1, "a")],
            vec![row(5, 1, json!({})), row(6, 1, json!({}))],
            &[split(5, 6)],
        );
        assert_eq!(state.now.tabs[0].id(), "w1:t5");
        let events = state.update(vec![space(1, "a")], vec![row(6, 1, json!({}))], &[]);
        assert_eq!(
            state.now.tabs[0].id(),
            "w1:t5",
            "the survivor keeps the tab's id"
        );
        assert_eq!(names(&events), ["pane.closed", "pane.exited"]);
        let layout = translate::layout(&state.now.tabs[0], AREA, None, false);
        assert_eq!(layout["panes"][0]["pane_id"], "w1:p6");

        // A new tab whose first leaf number is a key still in use takes another.
        state.update(
            vec![space(1, "a")],
            vec![
                row(6, 1, json!({})),
                row(5, 1, json!({})),
                row(7, 1, json!({})),
            ],
            &[],
        );
        let ids: Vec<String> = state.now.tabs.iter().map(Tab::id).collect();
        assert_eq!(ids, ["w1:t5", "w1:t1", "w1:t7"]);
    }

    #[test]
    fn revisions_rise_on_every_change_and_never_reset() {
        let mut state = State::default();
        let r = |s: &str| row(8, 1, json!({"state": s}));
        state.update(vec![space(1, "a")], vec![r("idle")], &[]);
        assert_eq!(state.revisions[&8], 1);
        state.update(vec![space(1, "a")], vec![r("idle")], &[]);
        assert_eq!(state.revisions[&8], 1, "nothing changed");
        state.update(vec![space(1, "a")], vec![r("working")], &[]);
        assert_eq!(state.revisions[&8], 2);
        state.update(vec![space(1, "a")], vec![], &[]);
        state.update(vec![space(1, "a")], vec![r("done")], &[]);
        assert_eq!(state.revisions[&8], 3, "kept across the gap");
        let panes = state.view().panes();
        assert_eq!(panes[0]["revision"], 3);
    }

    #[test]
    fn served_and_later_are_exactly_what_unknown_method_names() {
        let mut both: Vec<&str> = SERVED.iter().chain(LATER).copied().collect();
        both.sort_unstable();
        let mut methods = translate::METHODS.to_vec();
        methods.sort_unstable();
        assert_eq!(both, methods);
        assert!(SERVED.iter().all(|m| !LATER.contains(m)));
        assert!(!translate::METHODS.contains(&"pane.report_agent"));
    }

    #[tokio::test]
    async fn an_unknown_method_gets_herdrs_refusal() {
        let dir = tempfile::tempdir().expect("dir");
        let host = Host::new(test_home(dir.path()));
        let answer = host.answer("7", "pane.report_agent", &json!({})).await;
        assert_eq!(answer["id"], "7");
        assert_eq!(answer["error"]["code"], "invalid_request");
        let message = answer["error"]["message"].as_str().expect("message");
        assert!(message.starts_with(
            "invalid request: unknown variant `pane.report_agent`, expected one of `ping`"
        ));
        let ping = host.answer("8", "ping", &json!({})).await;
        assert_eq!(
            ping["result"],
            json!({"type": "pong", "version": "0.9.5", "protocol": 22})
        );
        let later = host.answer("9", "pane.split", &json!({})).await;
        assert_eq!(later["error"]["code"], "not_supported");
    }

    #[test]
    fn subscriptions_take_only_herdrs_types() {
        let ok = subscriptions(&json!({"subscriptions": [
            {"type": "pane.created"}, {"type": "pane.agent_status_changed", "pane_id": "w1:p2"}
        ]}))
        .expect("ok");
        assert_eq!(
            ok[1],
            ("pane.agent_status_changed".into(), Some("w1:p2".into()))
        );
        let err =
            subscriptions(&json!({"subscriptions": [{"type": "pane.nope"}]})).expect_err("no");
        assert!(err.contains("unknown variant `pane.nope`"), "{err}");
        assert!(subscriptions(&json!({})).is_err());
        let unbound =
            subscriptions(&json!({"subscriptions": [{"type": "pane.agent_status_changed"}]}));
        assert!(
            unbound
                .expect_err("needs a pane")
                .contains("needs a `pane_id`")
        );
    }

    #[test]
    fn hooks_run_for_their_own_event_only() {
        let m = manifest::Manifest::parse(
            "id = \"h\"\n\
             [[events]]\non = \"pane.closed\"\ncommand = [\"a\"]\n\
             [[events]]\non = \"pane.agent_status_changed\"\ncommand = [\"b\"]\n\
             [[events]]\non = \"tab.closed\"\ncommand = [\"c\"]\n",
        )
        .expect("parses");
        let run = |name| -> Vec<String> {
            hooks_for(&m.events, name)
                .iter()
                .map(|h| h.command[0].clone())
                .collect()
        };
        assert_eq!(run("pane_closed"), ["a"]);
        assert_eq!(run("pane_agent_status_changed"), ["b"]);
        assert_eq!(run("tab_closed"), ["c"]);
        assert!(run("pane_created").is_empty());
    }

    fn test_home(at: &Path) -> Home {
        Home::at(at.join("data/plugins"), at.join("config/plugins"))
    }

    #[test]
    fn a_handler_gets_the_plugin_env_and_dies_with_its_group_at_the_limit() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let mut env = install::plugin_env(&home, "t.h", &root);
        env.push(("HERDR_PLUGIN_EVENT".into(), "pane_closed".into()));
        let argv: Vec<String> = ["sh", "-c", "env > seen.txt; sleep 30 & sleep 30"]
            .map(String::from)
            .to_vec();
        let log = dir.path().join("log/host.log");
        let child = spawn(&root, &argv, env, &log).expect("spawned");
        let group = child.id() as i32;
        let started = Instant::now();
        assert!(
            !wait_bounded(child, Duration::from_millis(800)),
            "killed at the limit"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(200));
        // SAFETY: probing a group with signal 0 only.
        let alive = unsafe { libc::killpg(group, 0) } == 0;
        assert!(!alive, "the background sleep went with its group");
        let seen = std::fs::read_to_string(root.join("seen.txt")).expect("ran in its root");
        for want in [
            "HERDR_PLUGIN_EVENT=pane_closed",
            "HERDR_PLUGIN_ID=t.h",
            "HERDR_ENV=1",
            "HERDR_CONFIG_FILE=",
            "HERDR_SOCKET_PATH=",
        ] {
            assert!(seen.contains(want), "{want} missing");
        }
        assert!(!seen.lines().any(|l| l.starts_with("QOOP_")), "{seen}");
        assert!(wait_bounded(
            spawn(&root, &["true".to_string()], vec![], &log).expect("spawned"),
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn hooks_are_told_the_pane_tab_or_space_an_event_is_about() {
        let mut state = State::default();
        let mut web = space(2, "web");
        web.dir = Some("/src/web".into());
        let claude = row(
            9,
            2,
            json!({"agent": true, "program": "claude", "cwd": "/src/web/app"}),
        );
        state.update(vec![web.clone()], vec![claude.clone()], &[]);
        let events = state.update(
            vec![web.clone(), space(3, "docs")],
            vec![claude.clone(), row(4, 3, json!({"name": "notes"}))],
            &[],
        );
        let by = |dotted: &str| {
            events
                .iter()
                .find(|e| e.dotted == dotted)
                .unwrap_or_else(|| panic!("no {dotted}"))
                .scope
                .clone()
        };
        let created = by("pane.created");
        assert_eq!(created.context["focused_pane_id"], "w3:p4");
        assert_eq!(created.context["tab_label"], "notes");
        let space_made = by("workspace.created");
        assert_eq!(space_made.context["workspace_id"], "w3");
        assert_eq!(space_made.context["workspace_label"], "docs");
        let tab_made = by("tab.created");
        let env: BTreeMap<_, _> = tab_made.env.into_iter().collect();
        assert_eq!(env["HERDR_TAB_ID"], "w3:t4");
        assert_eq!(env["HERDR_WORKSPACE_ID"], "w3");

        // What closed is found in the listing it was in.
        let events = state.update(vec![web], vec![claude], &[]);
        let closed = events
            .iter()
            .find(|e| e.dotted == "tab.closed")
            .expect("tab closed");
        assert_eq!(closed.scope.context["tab_id"], "w3:t4");
        assert_eq!(closed.scope.context["workspace_label"], "docs");
        let space_closed = events
            .iter()
            .find(|e| e.dotted == "workspace.closed")
            .expect("space closed");
        let env: BTreeMap<_, _> = space_closed.scope.env.clone().into_iter().collect();
        assert_eq!(env["HERDR_WORKSPACE_ID"], "w3");

        let mut focused = State::default();
        let mut s = space(2, "web");
        s.dir = Some("/src/web".into());
        focused.update(
            vec![s],
            vec![row(9, 2, json!({"agent": true, "program": "claude"}))],
            &[],
        );
        focused.focused = Some(9);
        let scope = focused_scope(&focused);
        assert_eq!(scope.context["focused_pane_agent"], "claude");
        assert_eq!(scope.context["workspace_cwd"], "/src/web");
        let env: BTreeMap<_, _> = scope.env.into_iter().collect();
        assert_eq!(env["HERDR_PANE_ID"], "w2:p9");
        assert_eq!(env["HERDR_WORKSPACE_CWD"], "/src/web");

        // A closed pane in a space that stays is told as itself, not only as its space.
        let mut state = State::default();
        state.update(
            vec![space(1, "a")],
            vec![row(1, 1, json!({})), row(2, 1, json!({}))],
            &[],
        );
        let events = state.update(vec![space(1, "a")], vec![row(1, 1, json!({}))], &[]);
        let closed = events
            .iter()
            .find(|e| e.dotted == "pane.closed")
            .expect("closed");
        let env: BTreeMap<_, _> = closed.scope.env.clone().into_iter().collect();
        assert_eq!(env["HERDR_PANE_ID"], "w1:p2");
        assert_eq!(env["HERDR_TAB_ID"], "w1:t2");
    }

    #[tokio::test]
    async fn a_lagging_subscriber_is_told_events_were_lost() {
        let dir = tempfile::tempdir().expect("dir");
        let host = Host::with_backlog(test_home(dir.path()), 2);
        let (ours, theirs) = tokio::net::UnixStream::pair().expect("pair");
        let (_rd, wr) = theirs.into_split();
        let params = json!({"subscriptions": [{"type": "pane.created"}]});
        let streaming = {
            let host = host.clone();
            tokio::spawn(async move { stream_events(&host, &json!(41), &params, wr).await })
        };
        let mut lines = tokio::io::BufReader::new(ours).lines();
        let ack: Value =
            serde_json::from_str(&lines.next_line().await.expect("read").expect("ack"))
                .expect("json");
        assert_eq!(
            ack,
            json!({"id": 41, "result": {"type": "subscription_started"}})
        );
        for n in 0..10 {
            let event = Event::new(
                "pane.created",
                None,
                json!({"pane": {"pane_id": format!("w1:p{n}")}}),
            );
            host.events.send(Arc::new(event)).expect("a subscriber");
        }
        let lost: Value =
            serde_json::from_str(&lines.next_line().await.expect("read").expect("line"))
                .expect("json");
        assert_eq!(lost["id"], 41);
        assert_eq!(lost["error"]["code"], "events_lost");
        let next: Value =
            serde_json::from_str(&lines.next_line().await.expect("read").expect("line"))
                .expect("json");
        assert_eq!(
            next["event"], "pane_created",
            "the stream goes on after the loss"
        );
        streaming.abort();
    }

    #[test]
    fn no_host_is_wanted_without_a_plugin() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        assert!(!host_wanted(&home, &dir.path().join("herdr.sock")));
    }

    #[test]
    fn a_recorded_startup_is_ended_only_while_its_leader_is_the_same_process() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        let spawn_sleep = || {
            std::process::Command::new("sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .expect("sleep")
        };
        let (mut live, mut other) = (spawn_sleep(), spawn_sleep());
        let group = live.id() as i32;
        let started = qoop_pty::sys::start_time_of(qoop_pty::Pid(group)).expect("start time");
        let other_group = other.id() as i32;
        edit_pgids(&home, |groups| {
            groups.push(("t.a".into(), group, started));
            // A recorded group whose pid now belongs to some other process.
            groups.push(("t.b".into(), other_group, started.wrapping_add(1)));
        });
        end_startups(&home, Some("t.a"));
        assert!(live.wait().is_ok(), "ended");
        assert_eq!(
            read_pgids(&pgids_file(&home)).len(),
            1,
            "t.b is left for its own rm"
        );
        end_startups(&home, None);
        assert!(
            other.try_wait().expect("wait").is_none(),
            "a reused pid is never signalled"
        );
        assert!(read_pgids(&pgids_file(&home)).is_empty());
        let _ = other.kill();
        let _ = other.wait();
    }
}

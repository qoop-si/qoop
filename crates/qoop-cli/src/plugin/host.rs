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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
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
/// How long an `[[events]]` command may run before its process group is killed. herdr sets no
/// limit; qoop keeps one so a hook that hangs cannot stay forever.
const HANDLER_LIMIT: Duration = Duration::from_secs(600);
/// How long the host waits for a daemon that went away to answer again (a live upgrade
/// re-executes in place) before it leaves too.
const RECONNECT: Duration = Duration::from_secs(10);
/// How long a process table read is reused.
const PROCS_FRESH: Duration = Duration::from_millis(500);
/// The area layouts are drawn in until an interface says the size of its panes' area
/// (`qoop.focus`): rectangles are then the split tree's proportions of that.
pub(super) const AREA: Rect = Rect {
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
/// How long the history sizes read for pane reads are reused: a sidebar lists every few
/// seconds, and each read asks the daemon once per terminal.
const HISTORY_FRESH: Duration = Duration::from_secs(2);
/// How many `[[events]]` commands run at once, across every plugin, as in herdr; the rest wait
/// their turn rather than being dropped.
const HANDLERS_AT_ONCE: usize = 32;
/// Hook commands one plugin may have waiting for a turn; more are dropped and logged.
const WAITING_PER_PLUGIN: usize = 256;
/// How big a plugin's host log grows before it is moved aside.
const LOG_CAP: u64 = 1 << 20;

/// The socket's name, beside the daemon's in the session's verified 0700 directory.
pub(crate) const SOCKET: &str = paths::PLUGIN_HOST_SOCKET;

/// Where this session's host listens. Everything of qoop's own uses this and never
/// `HERDR_SOCKET_PATH`, so nothing qoop runs can reach a real herdr. Nothing is created here.
pub(crate) fn socket_path() -> PathBuf {
    paths::plugin_host_socket()
}

/// Where the shim asks: the socket the plugin was told, else the session's. Only the shim reads
/// `HERDR_SOCKET_PATH`, which the host itself set for the plugin.
pub(crate) fn shim_socket_path() -> PathBuf {
    std::env::var_os("HERDR_SOCKET_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(socket_path)
}

/// The methods that only read, answered here; with [`WRITES`] exactly `translate::METHODS`, so
/// the unknown-method text never names what is not served.
#[cfg(test)]
const SERVED: &[&str] = &[
    "ping",
    "session.snapshot",
    "pane.list",
    "pane.get",
    "pane.current",
    "pane.read",
    "pane.layout",
    "pane.process_info",
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

/// The methods that change something in qoop, answered by `writes`. With [`SERVED`] exactly
/// `translate::METHODS` (a test holds them to it).
const WRITES: &[&str] = &[
    "pane.split",
    "pane.close",
    "pane.focus",
    "pane.send_text",
    "pane.send_input",
    "pane.rename",
    "pane.zoom",
    "pane.resize",
    "pane.swap",
    "pane.focus_direction",
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
pub(super) struct Listing {
    pub(super) spaces: Vec<WorkspaceRow>,
    pub(super) rows: Vec<AgentRow>,
    pub(super) tabs: Vec<Tab>,
}

impl Listing {
    /// The listing with `hidden` terminals left out, as if not yet made: their rows go, their
    /// own tabs go, and they leave the trees of the tabs they sit in.
    fn without(&self, hidden: &BTreeSet<u32>) -> Listing {
        if hidden.is_empty() {
            return self.clone();
        }
        let shown = |t: u32| !hidden.contains(&t);
        Listing {
            spaces: self.spaces.clone(),
            rows: self
                .rows
                .iter()
                .filter(|r| shown(r.terminal))
                .cloned()
                .collect(),
            tabs: self
                .tabs
                .iter()
                .filter_map(|tab| {
                    let leaves: Vec<u32> =
                        tab.leaves.iter().copied().filter(|t| shown(*t)).collect();
                    let first = *leaves.first()?;
                    let tree = tab
                        .tree
                        .as_ref()
                        .and_then(|t| t.retain(&shown))
                        .filter(|t| matches!(t, Pane::Split { .. }));
                    Some(Tab {
                        workspace: tab.workspace,
                        key: if shown(tab.key) { tab.key } else { first },
                        leaves,
                        tree,
                    })
                })
                .collect(),
        }
    }
}

/// The herdr view of qoop the host keeps between listings.
#[derive(Default)]
pub(crate) struct State {
    pub(super) now: Listing,
    /// Per pane, raised on every change to its row and never reset: a reset reads as a new
    /// pane to a plugin that compares them.
    revisions: BTreeMap<u32, u64>,
    /// What plugins keep on panes and spaces with `report_metadata`, by terminal and space.
    pub(super) pane_tokens: BTreeMap<u32, super::tokens::Tokens>,
    pub(super) space_tokens: BTreeMap<i64, super::tokens::Tokens>,
    /// What each row's revision is compared on.
    seen: BTreeMap<u32, Value>,
    /// The terminal the interface shows, once it says (`qoop.focus`, or a `Focus` the daemon
    /// passes on), or the last one a plugin focused.
    pub(super) focused: Option<u32>,
    /// The space and pane the last focus events were about, so a change is told once.
    told_focus: Option<(i64, u32)>,
    /// Plugin panes opened as a popup or an overlay, each with the pane focused before it, which
    /// focus returns to when it closes.
    pub(super) popups: BTreeMap<u32, Popup>,
    /// Panes a plugin zoomed. qoop's zoom is the interface's own, so this is only what herdr
    /// answers about it.
    pub(super) zoomed: BTreeSet<u32>,
    /// Whether the interface has said what it shows (`qoop.focus`). Until it has, a call that
    /// names no pane and comes from no pane is refused rather than sent to a guess.
    pub(super) reported: bool,
    /// Panes whose popup went away on its own (its program ended): focus goes back to them.
    pub(super) restore: Vec<u32>,
    /// Terminals the host closed for a plugin: their going is `pane_closed`. Any other terminal
    /// that goes (its program ended, or a person closed it) is `pane_exited`, as a pane whose
    /// process ended is in herdr.
    pub(super) closing: BTreeSet<u32>,
    /// Terminals the host is still placing (made, not yet in the split it asked for): left
    /// out of what the events compare, so a split is told once, as herdr tells it, and not as
    /// a tab that came and went. `hidden_told` is the set the last comparison used.
    /// The cells the interface lays a tab out in, as it last said with `qoop.focus`.
    pub(super) area: Option<Rect>,
    /// Each pane's agent status as last listed, and herdr's `state_change_seq`: one count of
    /// status changes across every pane (`seq`), each pane holding the count of its last one.
    statuses: BTreeMap<u32, &'static str>,
    seq: u64,
    seqs: BTreeMap<u32, u64>,
    pub(super) hidden: BTreeSet<u32>,
    /// `now` as a plugin reads it ([`State::view`]).
    listed: Listing,
    hidden_told: BTreeSet<u32>,
    /// Terminals of a tab the host is closing with `tab.close`: herdr tells that as
    /// `tab_closed` alone, with no pane event, rename or layout on the way.
    pub(super) closing_tab: BTreeSet<u32>,
    /// Pane labels and agent names plugins gave (`pane.rename`, `agent.rename`, a plugin pane's
    /// title), kept in `.state/.labels.json` across restarts; `names_changed` until written.
    pub(super) names: translate::Names,
    pub(super) names_changed: bool,
    /// Whether a listing may prune `names`: not the first after the host starts or the daemon
    /// comes back, which may not hold every terminal yet.
    pub(super) prune_ready: bool,
    /// Rows kept behind each pane's screen (read with the panes, at most every
    /// [`HISTORY_FRESH`]) and the pane the interface shows scrolled back (`qoop.focus`).
    pub(super) scrolls: translate::Scrolls,
    history_at: Option<Instant>,
    /// The plugin that opened each plugin pane in this run of the host. Only a first filter:
    /// the pane's own program is asked too (`writes::own_pane`), and a restart forgets it.
    pub(super) owners: BTreeMap<u32, String>,
    /// Grants already used, by plugin pane: the person key (its time) each was given for.
    pub(super) grants_used: BTreeMap<u32, i64>,
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

/// The agent a row runs, by the name herdr would give it, when there is one.
fn agent_of(row: &AgentRow) -> Option<String> {
    translate::agent_name(row).map(str::to_string)
}

/// `pane.agent_status_changed`'s data: what is unknown or empty is left out, as herdr writes it.
fn status_data(id: &str, row: &AgentRow) -> Value {
    translate::compact(json!({
        "pane_id": id,
        "workspace_id": Id::Workspace(row.workspace).to_string(),
        "agent_status": translate::agent_status(row),
        "agent": agent_of(row),
        "display_agent": agent_of(row),
        "title": row.agent_title,
    }))
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
            // herdr counts an agent's status changes from when it is found: 1 at the first.
            if row.agent {
                let status = translate::agent_status(row);
                if self.statuses.insert(row.terminal, status) != Some(status) {
                    self.seq += 1;
                    self.seqs.insert(row.terminal, self.seq);
                }
            } else {
                self.statuses.remove(&row.terminal);
            }
            let now = compared(row);
            if self.seen.get(&row.terminal) != Some(&now) {
                *self.revisions.entry(row.terminal).or_insert(0) += 1;
                self.seen.insert(row.terminal, now);
            }
        }
        let old = std::mem::replace(&mut self.now, Listing { spaces, rows, tabs });
        // Names of terminals gone go with them; a sleeping tab's are kept for its wake.
        let listed: BTreeSet<u32> = self.now.rows.iter().map(|r| r.terminal).collect();
        let asleep = self
            .now
            .spaces
            .iter()
            .flat_map(|s| &s.sleeping)
            .map(|t| t.terminal);
        let kept: BTreeSet<u32> = listed.iter().copied().chain(asleep).collect();
        if std::mem::replace(&mut self.prune_ready, true) {
            let before = self.names.clone();
            self.names.panes.retain(|t, _| kept.contains(t));
            self.names.agents.retain(|t, _| kept.contains(t));
            self.names_changed |= before != self.names;
        }
        self.owners.retain(|t, _| listed.contains(t));
        self.grants_used.retain(|t, _| listed.contains(t));
        if !std::mem::replace(&mut self.primed, true) {
            self.listed = self.now.clone();
            self.told_focus = self.focus_pair();
            return Vec::new();
        }
        // A popup or a zoom whose pane has gone: forgotten, and focus handed back when it was
        // still on the popup (a plugin that focused elsewhere first keeps that, as in herdr,
        // whose popup floats over a focus it never took).
        let alive: BTreeSet<u32> = self.now.rows.iter().map(|r| r.terminal).collect();
        self.zoomed.retain(|t| alive.contains(t));
        self.pane_tokens.retain(|t, _| alive.contains(t));
        let spaces: BTreeSet<i64> = self.now.spaces.iter().map(|s| s.id).collect();
        self.space_tokens.retain(|s, _| spaces.contains(s));
        let gone: Vec<u32> = self
            .popups
            .keys()
            .filter(|t| !alive.contains(t))
            .copied()
            .collect();
        for popup in gone {
            let shown = self.focused == Some(popup);
            if let Some(previous) = self.popups.remove(&popup).and_then(|p| p.previous)
                && shown
                && alive.contains(&previous)
            {
                self.focused = Some(previous);
                self.restore.push(previous);
            }
        }
        // Compared as told: terminals still being placed, and popups, left out, on each side as
        // it was then.
        let unlisted = self.unlisted();
        let mut events = if unlisted.is_empty() && self.hidden_told.is_empty() {
            self.told(&old)
        } else {
            let told_old = old.without(&self.hidden_told);
            let told_new = self.now.without(&unlisted);
            self.hidden_told = unlisted;
            let full = std::mem::replace(&mut self.now, told_new);
            let events = self.told(&told_old);
            self.now = full;
            events
        };
        self.closing.retain(|t| alive.contains(t));
        self.closing_tab.retain(|t| alive.contains(t));
        // One listed before and gone now went before it was placed; one not listed yet stays.
        self.hidden
            .retain(|t| alive.contains(t) || !old.rows.iter().any(|r| r.terminal == *t));
        self.listed = self.now.without(&self.unlisted());
        events.extend(self.focus_events());
        events.extend(self.expire_tokens(Instant::now()));
        events
    }

    /// What changed from `old` to `now`, each event with its scope.
    fn told(&self, old: &Listing) -> Vec<Event> {
        let mut events = self.diff(old);
        for event in &mut events {
            event.scope = scope_of(event, &self.now, old, self.focused);
        }
        events
    }

    /// Tokens past their time removed, as herdr's runtime removes them: a pane's revision rises
    /// and both kinds are told as a report that changed them is.
    pub(super) fn expire_tokens(&mut self, now: Instant) -> Vec<Event> {
        let panes: Vec<u32> = (self.pane_tokens.iter_mut())
            .filter_map(|(t, held)| held.expire(now).then_some(*t))
            .collect();
        for terminal in &panes {
            *self.revisions.entry(*terminal).or_insert(0) += 1;
        }
        let spaces: Vec<i64> = (self.space_tokens.iter_mut())
            .filter_map(|(s, held)| held.expire(now).then_some(*s))
            .collect();
        self.token_events(&panes, &spaces)
    }

    /// `pane_updated` with the pane for each of `panes`, and `workspace_metadata_updated` with
    /// the space for each of `spaces`, whose tokens changed. Neither runs a plugin's hooks
    /// ([`NOT_HOOKABLE`]), so a hook that refreshes its own token cannot loop.
    pub(super) fn token_events(&self, panes: &[u32], spaces: &[i64]) -> Vec<Event> {
        let view = self.view();
        let mut out = Vec::new();
        if !panes.is_empty() {
            let infos = view.panes();
            for terminal in panes {
                if let Some(pane) = pane_of(infos.clone(), *terminal) {
                    let id = pane["pane_id"].as_str().map(str::to_string);
                    out.push(Event::new("pane.updated", id, json!({"pane": pane})));
                }
            }
        }
        if !spaces.is_empty() {
            let infos = view.workspace_infos();
            for space in spaces {
                let id = Id::Workspace(*space).to_string();
                if let Some(workspace) = infos.iter().find(|w| w["workspace_id"] == id) {
                    out.push(Event::new(
                        "workspace.metadata_updated",
                        None,
                        json!({"workspace": workspace}),
                    ));
                }
            }
        }
        out
    }

    /// The space and pane in focus, when the focused terminal is listed.)
    fn focus_pair(&self) -> Option<(i64, u32)> {
        let terminal = self.shown_focus()?;
        let row = self.now.rows.iter().find(|r| r.terminal == terminal)?;
        Some((row.workspace, terminal))
    }

    /// An interface's focus told again while it stands: taken when no interface has said what
    /// it shows, with no focus events and so no hooks; ignored otherwise. Whether `terminal`
    /// is listed.
    pub(super) fn keep_focus(&mut self, terminal: u32) -> bool {
        if !self.now.rows.iter().any(|r| r.terminal == terminal) {
            return false;
        }
        if !self.reported {
            self.focused = Some(terminal);
            self.reported = true;
            self.told_focus = self.focus_pair();
        }
        true
    }

    /// `workspace_focused`, `tab_focused` and `pane_focused`, in that order, when the focus
    /// moved since they were last told: herdr tells all three for any move, even within a tab.
    pub(super) fn focus_events(&mut self) -> Vec<Event> {
        let now = self.focus_pair();
        if now.is_none() || now == self.told_focus {
            return Vec::new();
        }
        self.told_focus = now;
        let (space, terminal) = now.expect("checked above");
        let workspace_id = Id::Workspace(space).to_string();
        let pane_id = Id::Pane {
            workspace: space,
            terminal,
        }
        .to_string();
        let mut out = vec![Event::new(
            "workspace.focused",
            None,
            json!({"workspace_id": workspace_id}),
        )];
        if let Some(tab) = self.now.tabs.iter().find(|t| t.leaves.contains(&terminal)) {
            out.push(Event::new(
                "tab.focused",
                None,
                json!({"tab_id": tab.id(), "workspace_id": workspace_id}),
            ));
        }
        out.push(Event::new(
            "pane.focused",
            Some(pane_id.clone()),
            json!({"pane_id": pane_id, "workspace_id": workspace_id}),
        ));
        for event in &mut out {
            event.scope = scope_of(event, &self.now, &self.now, self.focused);
        }
        out
    }

    /// What a plugin reads: popups and terminals still being placed left out, as herdr's
    /// popups are no panes, and the focus behind a popup taken as the focus.
    pub(crate) fn view(&self) -> View<'_> {
        self.view_of(&self.listed)
    }

    /// Terminals a plugin does not see: those being placed, and popups.
    pub(super) fn unlisted(&self) -> BTreeSet<u32> {
        let popups = self.popups.iter().filter(|(_, p)| p.popup).map(|(t, _)| *t);
        self.hidden.iter().copied().chain(popups).collect()
    }

    /// A space's focused pane: the one shown when it is in that space, else its first tab's first.
    pub(super) fn space_pane(&self, space: i64) -> Option<u32> {
        let here = |t: &u32| {
            self.now
                .rows
                .iter()
                .any(|r| r.terminal == *t && r.workspace == space)
        };
        self.shown_focus().filter(here).or_else(|| {
            let tab = self.listed.tabs.iter().find(|t| t.workspace == space)?;
            Some(tab.leaves[0])
        })
    }

    /// The focus as a plugin sees it: the pane behind a popup when a popup has it.
    pub(super) fn shown_focus(&self) -> Option<u32> {
        match self.focused {
            Some(t) if self.popups.get(&t).is_some_and(|p| p.popup) => self.popups[&t].previous,
            other => other,
        }
    }

    fn view_of<'a>(&'a self, listing: &'a Listing) -> View<'a> {
        View {
            spaces: &listing.spaces,
            rows: &listing.rows,
            tabs: &listing.tabs,
            focused: self.shown_focus(),
            revisions: &self.revisions,
            tokens: self.token_values(),
            area: self.area.unwrap_or(AREA),
            zoomed: self.zoomed.clone(),
            seqs: self.seqs.clone(),
            names: self.names.clone(),
            scrolls: self.scrolls.clone(),
        }
    }

    /// The tokens alive now, by pane and by space, for a view.
    pub(super) fn token_values(&self) -> translate::Tokens {
        let now = Instant::now();
        translate::Tokens {
            panes: self
                .pane_tokens
                .iter()
                .map(|(t, held)| (*t, held.values(now)))
                .collect(),
            spaces: self
                .space_tokens
                .iter()
                .map(|(s, held)| (*s, held.values(now)))
                .collect(),
        }
    }

    /// `layout_updated` for a tab, laid out as it is now.
    pub(super) fn layout_event(&self, tab: &Tab) -> Event {
        let zoomed = tab.leaves.iter().any(|t| self.zoomed.contains(t));
        Event::new(
            "layout.updated",
            None,
            json!({"layout": translate::layout(tab, self.area.unwrap_or(AREA), self.focused, zoomed)}),
        )
    }

    fn diff(&self, old: &Listing) -> Vec<Event> {
        let new = &self.now;
        let (view, before) = (self.view_of(&self.now), self.view_of(old));
        let find = |list: &[Value], key: &str, id: &str| {
            list.iter()
                .find(|v| v[key] == id)
                .cloned()
                .unwrap_or(Value::Null)
        };
        let (workspaces, tabs, panes) = (view.workspace_infos(), view.tab_infos(), view.panes());
        let old_workspaces = before.workspace_infos();
        let ws = |id: i64| Id::Workspace(id).to_string();
        let pane_id = |row: &AgentRow| {
            Id::Pane {
                workspace: row.workspace,
                terminal: row.terminal,
            }
            .to_string()
        };
        let tab_of = |listing: &'_ Listing, t: u32| -> Option<Tab> {
            listing
                .tabs
                .iter()
                .find(|tab| tab.leaves.contains(&t))
                .cloned()
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

        let mut layouts = Vec::new();
        for tab in &new.tabs {
            let id = tab.id();
            let info = find(&tabs, "tab_id", &id);
            match old.tabs.iter().find(|o| o.key == tab.key) {
                None => {
                    out.push(Event::new("tab.created", None, json!({"tab": info})));
                    layouts.push(self.layout_event(tab));
                }
                Some(was) => {
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
                    // A tab on its way out with `tab.close` says nothing until it is gone.
                    let closing = was.leaves.iter().any(|t| self.closing_tab.contains(t));
                    // Renamed when its own name changed (`tab.rename`), never because a number moved.
                    let renamed =
                        translate::tab_name(was, &old.rows) != translate::tab_name(tab, &new.rows);
                    if renamed && !closing {
                        out.push(Event::new(
                            "tab.renamed",
                            None,
                            json!({"tab_id": id, "workspace_id": ws(tab.workspace),
                                   "label": info["label"]}),
                        ));
                    }
                    // A pane added, or one gone from a tab that keeps others, as herdr tells a
                    // layout.
                    if was.leaves != tab.leaves && !closing {
                        layouts.push(self.layout_event(tab));
                    }
                }
            }
        }

        for row in &new.rows {
            let id = pane_id(row);
            let pane = find(&panes, "pane_id", &id);
            match old.rows.iter().find(|o| o.terminal == row.terminal) {
                None => {
                    out.push(Event::new(
                        "pane.created",
                        Some(id.clone()),
                        json!({"pane": pane}),
                    ));
                    // A pane that already runs an agent when first listed (one started
                    // with it): herdr tells the agent right after the pane.
                    if let Some(agent) = agent_of(row) {
                        out.push(Event::new(
                            "pane.agent_detected",
                            Some(id.clone()),
                            json!({"pane_id": id, "workspace_id": ws(row.workspace),
                                   "agent": agent}),
                        ));
                    }
                }
                Some(was) => {
                    // An agent arriving, leaving or changing, never a pane being made.
                    let (before, after) = (agent_of(was), agent_of(row));
                    if before != after {
                        let data = match (&before, &after) {
                            (Some(left), None) => json!({
                                "pane_id": id, "workspace_id": ws(row.workspace),
                                "agent": left, "released": true,
                                "final_status": translate::agent_status(was),
                            }),
                            _ => translate::compact(json!({
                                "pane_id": id, "workspace_id": ws(row.workspace),
                                "agent": after,
                            })),
                        };
                        out.push(Event::new("pane.agent_detected", Some(id.clone()), data));
                    }
                    let (was_data, data) = (status_data(&id, was), status_data(&id, row));
                    let presentation =
                        |d: &Value| json!([d["agent_status"], d["display_agent"], d["title"]]);
                    // An agent just found is told by `pane_agent_detected` alone while its state
                    // is still unknown: herdr tells its first status once it has one.
                    let unsettled = before.is_none()
                        && after.is_some()
                        && translate::agent_status(row) == "unknown";
                    if presentation(&was_data) != presentation(&data) && !unsettled {
                        out.push(Event::new(
                            "pane.agent_status_changed",
                            Some(id.clone()),
                            data,
                        ));
                    }
                    let (was_tab, tab) = (tab_of(old, was.terminal), tab_of(new, row.terminal));
                    let key = |t: &Option<Tab>| t.as_ref().map(|t| t.key);
                    if was.workspace != row.workspace || key(&was_tab) != key(&tab) {
                        let previous_tab = was_tab.map(|t| t.id()).unwrap_or_default();
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
            // The panes of a tab closed with `tab.close` go with it, told as `tab_closed` alone.
            if !new.rows.iter().any(|r| r.terminal == was.terminal)
                && !self.closing_tab.contains(&was.terminal)
            {
                let data = json!({"pane_id": pane_id(was), "workspace_id": ws(was.workspace)});
                // Closed through the host, or gone on its own: one of the two, never both.
                let name = if self.closing.contains(&was.terminal) {
                    "pane.closed"
                } else {
                    "pane.exited"
                };
                out.push(Event::new(name, Some(pane_id(was)), data));
            }
        }
        out.extend(layouts);
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
                let id = ws(was.id);
                out.push(Event::new(
                    "workspace.closed",
                    None,
                    json!({"workspace_id": id,
                           "workspace": find(&old_workspaces, "workspace_id", &id)}),
                ));
            }
        }
        out
    }
}

/// A plugin pane opened as a popup or an overlay.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Popup {
    /// The pane focused before it, where focus goes back when it closes.
    pub(super) previous: Option<u32>,
    pub(super) plugin: String,
    /// A popup, not an overlay: only a popup blocks another and closes on `popup.close`.
    pub(super) popup: bool,
}

/// A herdr refusal: its code and message.
pub(super) type Refused = (&'static str, String);

pub(super) fn not_found(what: &'static str, id: &str) -> Refused {
    let code = match what {
        "pane" => "pane_not_found",
        "tab" => "tab_not_found",
        "agent" => "agent_not_found",
        _ => "workspace_not_found",
    };
    (code, format!("{what} {id} not found"))
}

/// The pane of `panes` that is `terminal`.
pub(super) fn pane_of(panes: Vec<Value>, terminal: u32) -> Option<Value> {
    let id = terminal.to_string();
    panes
        .into_iter()
        .find(|p| p["terminal_id"].as_str() == Some(id.as_str()))
}

/// The terminal a pane id (or a bare terminal number) names.
pub(super) fn terminal_of(text: &str) -> Option<u32> {
    match Id::parse(text) {
        Some(Id::Pane { terminal, .. }) => Some(terminal),
        _ => text.parse().ok().filter(|n| *n > 0),
    }
}

pub(super) fn param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(Value::as_str)
}

/// The host: its view of qoop, its subscribers and its plugins.
pub(crate) struct Host {
    pub(super) home: Home,
    pub(super) state: Mutex<State>,
    /// Plugin pane specs waiting for their `pane-exec`, by nonce.
    pub(super) specs: super::writes::Specs,
    /// Held across a listing's round trips and its update, so listings apply in order.
    pub(super) refreshing: tokio::sync::Mutex<()>,
    pub(super) events: broadcast::Sender<Arc<Event>>,
    procs: Mutex<Option<(Instant, Arc<Vec<ProcRow>>)>>,
    /// When the last listing was applied, so a burst of requests lists the daemon once.
    listed_at: Mutex<Option<Instant>>,
    /// Turns for `[[events]]` commands, [`HANDLERS_AT_ONCE`] of them.
    hooks: Arc<tokio::sync::Semaphore>,
    /// Hook commands waiting for a turn, by plugin, at most [`WAITING_PER_PLUGIN`] each.
    waiting: Arc<Mutex<HashMap<String, usize>>>,
    /// The last `plugin-log-N` an action was given.
    pub(super) log_ids: std::sync::atomic::AtomicU64,
}

impl Host {
    pub(super) fn new(home: Home) -> Arc<Self> {
        Self::with_backlog(home, BACKLOG)
    }

    fn with_backlog(home: Home, backlog: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                names: load_names(&home),
                ..State::default()
            }),
            home,
            refreshing: tokio::sync::Mutex::new(()),
            events: broadcast::channel(backlog).0,
            procs: Mutex::new(None),
            listed_at: Mutex::new(None),
            specs: super::writes::Specs::default(),
            hooks: Arc::new(tokio::sync::Semaphore::new(HANDLERS_AT_ONCE)),
            waiting: Arc::default(),
            log_ids: std::sync::atomic::AtomicU64::new(0),
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
        let (events, restore) = {
            let mut state = self.state.lock().unwrap();
            let events = state.update(workspaces, agents, &splits);
            if std::mem::take(&mut state.names_changed) {
                save_names(&self.home, &state.names);
            }
            (events, std::mem::take(&mut state.restore))
        };
        for event in events {
            let _ = self.events.send(Arc::new(event));
        }
        *self.listed_at.lock().unwrap() = Some(Instant::now());
        // A popup that ended on its own hands focus back, as one closed by a call does.
        for terminal in restore {
            let _ = super::writes::control(ControlRequest::Focus {
                id: crate::request_id("plugin-host"),
                terminal,
                zoom: Some(false),
            })
            .await;
        }
        Ok(())
    }

    /// Take the last listing as stale, so the next read lists the daemon again.
    pub(super) fn forget_listing(&self) {
        *self.listed_at.lock().unwrap() = None;
    }

    /// A fresh listing on a connection of its own, for a request about to read the state. One
    /// applied in the last [`FRESH`] is fresh enough; with no daemon to ask (a live upgrade
    /// between its two halves), the last listing answers.
    pub(super) async fn refresh(&self) -> Result<(), Refused> {
        if self
            .listed_at
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() < FRESH)
        {
            return Ok(());
        }
        let primed = || self.state.lock().unwrap().primed;
        let unavailable = |e: std::io::Error| ("server_not_running", e.to_string());
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
    pub(super) fn with_view<R>(&self, read: impl FnOnce(&View) -> R) -> R {
        let (mut listing, revisions, focused, tokens, area, zoomed, seqs, names, scrolls) = {
            let state = self.state.lock().unwrap();
            (
                state.listed.clone(),
                state.revisions.clone(),
                state.shown_focus(),
                state.token_values(),
                state.area.unwrap_or(AREA),
                state.zoomed.clone(),
                state.seqs.clone(),
                state.names.clone(),
                state.scrolls.clone(),
            )
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
            tokens,
            area,
            zoomed,
            seqs,
            names,
            scrolls,
        })
    }

    /// How many rows each listed terminal keeps behind its screen, asked of the daemon at
    /// most every [`HISTORY_FRESH`]: what a pane read's `scroll` says it can go back.
    async fn read_histories(&self) {
        let terminals: Vec<u32> = {
            let state = self.state.lock().unwrap();
            if state
                .history_at
                .is_some_and(|at| at.elapsed() < HISTORY_FRESH)
            {
                return;
            }
            state.now.rows.iter().map(|r| r.terminal).collect()
        };
        let Ok(mut link) = Link::open_observer("qoop-plugin-host").await else {
            return;
        };
        let mut history = BTreeMap::new();
        for terminal in terminals {
            // From as far back as there is, no rows: the answer says where that is.
            let ask = ControlRequest::Scrollback {
                id: crate::request_id("plugin-host"),
                terminal,
                up: usize::MAX,
                rows: 0,
            };
            if let Ok((ControlResponse::Scrollback { from, total, .. }, _)) = link.ask(&ask).await {
                history.insert(terminal, total.saturating_sub(from));
            }
        }
        let mut state = self.state.lock().unwrap();
        state.scrolls.history = history;
        state.history_at = Some(Instant::now());
    }

    /// One request, answered in herdr's envelope.
    async fn answer(&self, id: &str, method: &str, params: &Value) -> Value {
        // qoop's own, never among herdr's methods: the interface's focus and a pane's spec.
        if method.starts_with("qoop.") {
            return match self.internal(method, params).await {
                Ok(result) => translate::reply(id, result),
                Err((code, message)) => translate::refuse(id, code, &message),
            };
        }
        if !translate::METHODS.contains(&method) {
            return translate::unknown_method(id, method);
        }
        match self.result(method, params).await {
            Ok(result) => translate::reply(id, result),
            Err((code, message)) => translate::refuse(id, code, &message),
        }
    }

    /// `pane.report_metadata` and `workspace.report_metadata`: the tokens kept for the pane or
    /// space named, read back as its `tokens`; a pane's revision rises when they change.
    async fn report_metadata(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        self.refresh().await?;
        let now = Instant::now();
        let events = {
            let mut state = self.state.lock().unwrap();
            Self::report_tokens(&mut state, method, params, now)?
        };
        for event in events {
            let _ = self.events.send(Arc::new(event));
        }
        Ok(json!({"type": "ok"}))
    }

    /// Apply one report to `state`; the events to tell when tokens changed.
    fn report_tokens(
        state: &mut State,
        method: &str,
        params: &Value,
        now: Instant,
    ) -> Result<Vec<Event>, Refused> {
        if method == "pane.report_metadata" {
            let id = param(params, "pane_id").unwrap_or_default();
            let terminal = terminal_of(id)
                .filter(|t| state.now.rows.iter().any(|r| r.terminal == *t))
                .ok_or_else(|| not_found("pane", id))?;
            let held = state.pane_tokens.entry(terminal).or_default();
            if super::tokens::report_pane(held, params, now)? {
                *state.revisions.entry(terminal).or_insert(0) += 1;
                return Ok(state.token_events(&[terminal], &[]));
            }
        } else {
            let id = param(params, "workspace_id").unwrap_or_default();
            let space = match Id::parse(id) {
                Some(Id::Workspace(n)) => Some(n),
                _ => None,
            }
            .filter(|n| state.now.spaces.iter().any(|s| s.id == *n))
            .ok_or_else(|| not_found("workspace", id))?;
            if super::tokens::report_space(
                state.space_tokens.entry(space).or_default(),
                params,
                now,
            )? {
                return Ok(state.token_events(&[], &[space]));
            }
        }
        Ok(Vec::new())
    }

    async fn result(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        if WRITES.contains(&method) {
            // Boxed: every write's state in one future is more stack than a worker has in a
            // debug build.
            return Box::pin(self.write(method, params)).await;
        }
        match method {
            "ping" => {
                // What this host does of what herdr's server says it can: it answers health
                // checks and runs detached; no handoff, ssh agent or surface registration.
                return Ok(json!({"type": "pong", "version": translate::VERSION,
                                 "protocol": translate::PROTOCOL, "capabilities": {
                                     "detached_server_daemon": true,
                                     "endpoint_protocol_generation": 1,
                                     "health_check": true, "live_handoff": false,
                                     "ssh_agent_registration": false,
                                     "surface_interest": false}}));
            }
            "server.reload_config" => {
                return Ok(
                    json!({"type": "config_reload", "status": "applied", "diagnostics": []}),
                );
            }
            "pane.report_metadata" | "workspace.report_metadata" => {
                return self.report_metadata(method, params).await;
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
        if matches!(
            method,
            "pane.list" | "pane.get" | "pane.current" | "session.snapshot"
        ) {
            self.read_histories().await;
        }
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
        // As herdr reads: `recent` when not said, at most 1000 lines, 80 of the history when
        // not said, the whole screen for `visible` and `detection`.
        let source = param(params, "source").unwrap_or("recent").to_string();
        let lines = params
            .get("lines")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).min(1000));
        let unavailable = |e: std::io::Error| ("server_not_running", e.to_string());
        let snapshot = crate::fetch_snapshot(terminal, "qoop-plugin-host")
            .await
            .map_err(unavailable)?;
        let (rows, truncated) = if matches!(source.as_str(), "visible" | "detection") {
            let mut rows = snapshot.lines;
            while rows.last().is_some_and(|r| r.trim().is_empty()) {
                rows.pop();
            }
            let keep = lines.unwrap_or(rows.len()).min(rows.len());
            let truncated = keep < rows.len();
            (rows.split_off(rows.len() - keep), truncated)
        } else {
            // The last `n` rows written, the screen included, as herdr reads them: what went
            // past is read from the history only for the rows the screen does not hold.
            let n = lines.unwrap_or(80);
            let alt = snapshot.modes.alt_screen;
            let held = if alt {
                0
            } else {
                screen_end(&snapshot.lines, snapshot.cursor.0).map_or(0, |end| end + 1)
            };
            // Never further back than there is: asked for more, the read starts at the oldest
            // row and comes back short of the newest.
            let up = n.saturating_sub(held).min(snapshot.behind as usize);
            let history = if up == 0 || alt {
                Vec::new()
            } else {
                let mut link = Link::open_observer("qoop-plugin-host")
                    .await
                    .map_err(unavailable)?;
                let ask = ControlRequest::Scrollback {
                    id: crate::request_id("plugin-host"),
                    terminal,
                    up,
                    rows: up,
                };
                match link.ask(&ask).await.map_err(unavailable)?.0 {
                    ControlResponse::Scrollback { lines, .. } => lines,
                    ControlResponse::Failed { reason, .. } => return Err(("read_failed", reason)),
                    _ => return Err(("read_failed", "unexpected answer to a history read".into())),
                }
            };
            let total = snapshot.behind as usize + snapshot.rows;
            let rows = recent_rows(history, &snapshot.lines, snapshot.cursor.0, alt, n);
            let rows = if source == "recent_unwrapped" {
                unwrapped(rows, snapshot.cols)
            } else {
                rows
            };
            (rows, total > n)
        };
        Ok(json!({"type": "pane_read", "read": {
            "pane_id": pane["pane_id"], "workspace_id": pane["workspace_id"],
            "tab_id": pane["tab_id"], "source": source,
            "format": param(params, "format").unwrap_or("text"),
            // herdr ends every read with a newline but an unwrapped one, read as one span.
            "text": match (rows.is_empty(), source.as_str()) {
                (true, _) => String::new(),
                (false, "recent_unwrapped") => rows.join("\n"),
                (false, _) => rows.join("\n") + "\n",
            },
            "revision": pane["revision"], "truncated": truncated,
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
            Ok(json!({"type": "session_snapshot", "snapshot": view.snapshot(view.area)}))
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
            // The pane the call comes from, when it says; the focused pane only without one.
            let terminal = match param(params, "caller_pane_id") {
                Some(caller) => terminal_of(caller),
                None => view.focused,
            }
            .ok_or(("pane_not_found", "pane not found".to_string()))?;
            let pane = pane_of(view.panes(), terminal)
                .ok_or_else(|| not_found("pane", &terminal.to_string()))?;
            Ok(json!({"type": "pane_current", "pane": pane}))
        }
        "pane.layout" => {
            let asked = param(params, "pane_id");
            let terminal = asked.and_then(terminal_of).or(view.focused);
            let tab = view
                .tabs
                .iter()
                .find(|t| terminal.is_some_and(|x| t.leaves.contains(&x)))
                .ok_or_else(|| not_found("pane", asked.unwrap_or("current")))?;
            let layout = translate::layout(tab, view.area, view.focused, view.zoomed_tab(tab));
            Ok(json!({"type": "pane_layout", "layout": layout}))
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
        _ => Err((
            "invalid_request",
            format!("invalid request: unknown variant `{method}`"),
        )),
    }
}

/// herdr's `InstalledPluginInfo` for each plugin here (or the one asked for).
pub(super) fn plugins(home: &Home, only: Option<&str>) -> Vec<Value> {
    install::installed(home)
        .into_iter()
        .filter(|id| only.is_none_or(|o| o == id))
        .map(|id| {
            let checked = install::admitted(home, &id);
            let checkout = home
                .dir_of(&id)
                .canonicalize()
                .unwrap_or_else(|_| home.dir_of(&id));
            let (root, source) = match &checked {
                Ok(a) => (a.root.clone(), source_info(a, &checkout)),
                Err(_) => (checkout.clone(), json!({"kind": "local"})),
            };
            let mut info = json!({
                "plugin_id": id, "name": id, "version": "",
                "manifest_path": root.join(manifest::FILE), "plugin_root": root,
                "enabled": checked.is_ok(), "source": source,
            });
            if let Ok(a) = &checked {
                described(&mut info, &a.manifest);
            }
            info
        })
        .collect()
}

/// What herdr's `plugin.list` says of a plugin's manifest, every entry as written (herdr lists
/// them all, whatever platform they are for); empty lists and absent values left out.
fn described(info: &mut Value, m: &manifest::Manifest) {
    let steps = |steps: &[manifest::Step]| -> Vec<Value> {
        steps
            .iter()
            .map(|s| translate::compact(json!({"platforms": s.platforms, "command": s.command})))
            .collect()
    };
    let fields = json!({
        "name": m.name, "version": m.version.clone().unwrap_or_default(),
        "min_herdr_version": m.min_herdr_version.clone().unwrap_or_default(),
        "description": m.description, "platforms": m.platforms,
        "build": steps(&m.build), "startup": steps(&m.startup),
        "actions": m.actions.iter().map(|a| translate::compact(json!({
            "id": a.id, "title": a.title.clone().unwrap_or_else(|| a.id.clone()),
            "description": a.description, "contexts": a.contexts,
            "platforms": a.platforms, "command": a.command,
        }))).collect::<Vec<_>>(),
        "events": m.events.iter().map(|h| translate::compact(json!({
            "on": h.on, "platforms": h.platforms, "command": h.command,
        }))).collect::<Vec<_>>(),
        "panes": m.panes.iter().map(|p| translate::compact(json!({
            "id": p.id, "title": p.title.clone().unwrap_or_else(|| p.id.clone()),
            "description": p.description, "platforms": p.platforms,
            "placement": p.placement.clone().unwrap_or_else(|| "overlay".into()),
            "width": p.width, "height": p.height, "command": p.command,
        }))).collect::<Vec<_>>(),
        "link_handlers": m.link_handlers.iter().map(|l| translate::compact(json!({
            "id": l.id, "title": l.title, "pattern": l.pattern, "action": l.action,
            "platforms": l.platforms,
        }))).collect::<Vec<_>>(),
    });
    if let (Value::Object(info), Value::Object(fields)) = (info, fields) {
        for (key, value) in fields {
            let empty = value.is_null() || value.as_array().is_some_and(Vec::is_empty);
            if !empty {
                info.insert(key, value);
            }
        }
    }
}

/// herdr's `PluginSourceInfo` from what was agreed to.
fn source_info(plugin: &Admitted, checkout: &Path) -> Value {
    let Some(sha) = &plugin.consent.sha else {
        return json!({"kind": "local"});
    };
    let mut parts = plugin.consent.source.splitn(3, '/');
    let (owner, repo, subdir) = (parts.next(), parts.next(), parts.next());
    json!({"kind": "github", "managed_path": checkout, "owner": owner, "repo": repo,
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
                let mut info = json!({"plugin_id": id, "action_id": action.id,
                           "title": action.title.unwrap_or_else(|| action.id.clone()),
                           "command": action.command, "contexts": action.contexts,
                           "description": action.description, "platforms": action.platforms});
                // As herdr writes them: no empty contexts, nothing absent.
                if let Value::Object(fields) = &mut info {
                    fields
                        .retain(|_, v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()));
                }
                info
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

impl Scope {
    /// A scope of just these context fields (what herdr tells about something closed).
    fn bare(fields: &[(&str, Option<String>)]) -> Self {
        let context: serde_json::Map<String, Value> = fields
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), json!(v.clone()?))))
            .collect();
        Self::of(Value::Object(context))
    }

    /// A scope from its context: the ids a command is told come from the context, as herdr
    /// sets them.
    pub(super) fn of(context: Value) -> Self {
        let env = env_of_context(&context);
        Self { context, env }
    }
}

/// `HERDR_WORKSPACE_ID`, `_TAB_ID`, `_PANE_ID` from a context's ids, and qoop's
/// `HERDR_WORKSPACE_CWD` from its `workspace_cwd`.
pub(super) fn env_of_context(context: &Value) -> Vec<(String, OsString)> {
    [
        ("HERDR_WORKSPACE_ID", "workspace_id"),
        ("HERDR_TAB_ID", "tab_id"),
        ("HERDR_PANE_ID", "focused_pane_id"),
        ("HERDR_WORKSPACE_CWD", "workspace_cwd"),
    ]
    .into_iter()
    .filter_map(|(var, key)| Some((var.to_string(), context.get(key)?.as_str()?.into())))
    .collect()
}

/// The id an event's data names under `key`, directly or inside its `object`.
fn named<'a>(data: &'a Value, key: &str, object: &str) -> Option<&'a str> {
    data.get(key)
        .or_else(|| data.get(object).and_then(|o| o.get(key)))
        .and_then(Value::as_str)
}

/// The pane a set of terminals stands for: the focused one when it is among them, else the
/// first.
fn shown(focused: Option<u32>, terminals: &[u32]) -> Option<u32> {
    focused
        .filter(|t| terminals.contains(t))
        .or_else(|| terminals.first().copied())
}

/// The scope of an event, as herdr tells a hook: a pane event its pane; a tab event the tab
/// with its shown pane; a space event the space with its shown tab and pane. What has closed
/// is told by its ids only (a space by its label and last tab too), from the listing it was in.
fn scope_of(event: &Event, new: &Listing, old: &Listing, focused: Option<u32>) -> Scope {
    let data = &event.data;
    let id = |key: &str, object: &str| named(data, key, object).map(str::to_string);
    match event.dotted {
        "pane.closed" => {
            return Scope::bare(&[
                ("workspace_id", id("workspace_id", "workspace")),
                ("focused_pane_id", id("pane_id", "pane")),
            ]);
        }
        "tab.closed" => {
            return Scope::bare(&[
                ("workspace_id", id("workspace_id", "workspace")),
                ("tab_id", id("tab_id", "tab")),
            ]);
        }
        "workspace.closed" => {
            let space = id("workspace_id", "workspace");
            let parsed = space.as_deref().and_then(Id::parse);
            let label = old
                .spaces
                .iter()
                .find(|s| Some(Id::Workspace(s.id)) == parsed)
                .map(|s| s.name.clone());
            let tabs: Vec<&Tab> = old
                .tabs
                .iter()
                .filter(|t| Some(Id::Workspace(t.workspace)) == parsed)
                .collect();
            let tab = tabs
                .iter()
                .find(|t| focused.is_some_and(|f| t.leaves.contains(&f)))
                .or(tabs.first())
                .map(|t| t.id());
            return Scope::bare(&[
                ("workspace_id", space),
                ("workspace_label", label),
                ("tab_id", tab),
            ]);
        }
        _ => {}
    }
    let terminal = named(data, "pane_id", "pane").and_then(terminal_of);
    for listing in [new, old] {
        if let Some(row) = terminal.and_then(|t| listing.rows.iter().find(|r| r.terminal == t)) {
            return scope(listing, row.workspace, Some(row.terminal));
        }
    }
    let tab_id = named(data, "tab_id", "tab");
    for listing in [new, old] {
        if let Some(tab) = tab_id.and_then(|id| listing.tabs.iter().find(|t| t.id() == id)) {
            return scope(listing, tab.workspace, shown(focused, &tab.leaves));
        }
    }
    let space = named(data, "workspace_id", "workspace").and_then(Id::parse);
    for listing in [new, old] {
        if let Some(Id::Workspace(space)) = space
            && listing.spaces.iter().any(|s| s.id == space)
        {
            let terminals: Vec<u32> = listing
                .rows
                .iter()
                .filter(|r| r.workspace == space)
                .map(|r| r.terminal)
                .collect();
            return scope(listing, space, shown(focused, &terminals));
        }
    }
    Scope::of(json!({}))
}

/// The scope of a space and, when given, one pane in it. Fields with nothing in them are left
/// out, as herdr leaves them out. The space's folder is its focused pane's, else its own.
pub(super) fn scope(listing: &Listing, workspace: i64, terminal: Option<u32>) -> Scope {
    let space = listing.spaces.iter().find(|s| s.id == workspace);
    let row = terminal.and_then(|t| listing.rows.iter().find(|r| r.terminal == t));
    let tab = terminal.and_then(|t| listing.tabs.iter().find(|tab| tab.leaves.contains(&t)));
    // Its name, else its number in its space, as herdr labels a tab.
    let tab_label = tab.map(|tab| {
        let number = listing
            .tabs
            .iter()
            .filter(|t| t.workspace == tab.workspace)
            .position(|t| t.key == tab.key)
            .map_or(1, |i| i + 1);
        translate::tab_label(tab, &listing.rows, number)
    });
    let workspace_cwd = row
        .and_then(|r| r.cwd.clone())
        .or_else(|| space.and_then(|s| s.dir.clone()));
    let pane_id = row.map(|r| {
        Id::Pane {
            workspace: r.workspace,
            terminal: r.terminal,
        }
        .to_string()
    });
    Scope::of(translate::compact(json!({
        "workspace_id": Id::Workspace(workspace).to_string(),
        "workspace_label": space.map(|s| translate::space_label(s, &listing.rows)),
        "workspace_cwd": workspace_cwd,
        "tab_id": tab.map(Tab::id),
        "tab_label": tab_label,
        "focused_pane_id": pane_id,
        "focused_pane_cwd": row.and_then(|r| r.cwd.clone()),
        "focused_pane_agent": row.and_then(translate::agent_name),
        "focused_pane_status": row.map(translate::agent_status),
    })))
}

/// The scope of what the interface shows, when it has said; for a startup command or an
/// action.
pub(super) fn focused_scope(state: &State) -> Scope {
    match state
        .focused
        .and_then(|t| state.now.rows.iter().find(|r| r.terminal == t))
    {
        Some(row) => scope(&state.now, row.workspace, Some(row.terminal)),
        None => Scope::of(json!({})),
    }
}

/// `context` with how the command came to run, as herdr marks it.
pub(super) fn invoked(context: &Value, source: &str, correlation: &str) -> Value {
    let mut context = context.clone();
    if let Value::Object(fields) = &mut context {
        fields
            .entry("invocation_source")
            .or_insert_with(|| json!(source));
        fields
            .entry("correlation_id")
            .or_insert_with(|| json!(correlation));
    }
    context
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
pub(super) fn note(home: &Home, id: &str, line: &str) {
    if std::fs::symlink_metadata(home.dir_of(id)).is_err() {
        return;
    }
    if let Ok(mut file) = open_log(&home.state_of(id).join("host.log")) {
        let _ = writeln!(file, "qoop plugin host: {line}");
    }
}

/// Run one plugin command in its root with its environment, in a process group of its own,
/// output appended to `log`. The caller bounds it ([`wait_bounded`]) or records its group.
pub(super) fn spawn(
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

/// Where the pane labels and agent names plugins gave are kept.
fn names_file(home: &Home) -> PathBuf {
    home.state_of(".labels.json")
}

/// The names kept from before, none when there are none or they cannot be read.
pub(super) fn load_names(home: &Home) -> translate::Names {
    std::fs::read(names_file(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Keep `names`, written whole beside the old file and moved over it.
pub(super) fn save_names(home: &Home, names: &translate::Names) {
    let file = names_file(home);
    let Some(dir) = file.parent() else {
        return;
    };
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| tempfile::NamedTempFile::new_in(dir))
        .and_then(|mut tmp| {
            serde_json::to_writer(&mut tmp, names).map_err(std::io::Error::other)?;
            tmp.persist(&file).map(drop).map_err(|e| e.error)
        });
    if let Err(e) = written {
        eprintln!("qoop plugin host: the pane names could not be kept: {e}");
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

/// Events herdr never runs a hook for.
const NOT_HOOKABLE: &[&str] = &[
    "layout.updated",
    "pane.output_changed",
    "pane.updated",
    "workspace.metadata_updated",
];

impl Host {
    /// Run every plugin's `[[events]]` for one event, at most [`HANDLERS_AT_ONCE`] across all
    /// plugins (the rest wait their turn), each ended at [`HANDLER_LIMIT`].
    fn run_hooks(&self, event: &Event) {
        if NOT_HOOKABLE.contains(&event.dotted) {
            return;
        }
        let Some((name, payload)) = event.for_hook() else {
            return;
        };
        let context = invoked(&event.scope.context, "api", event.dotted);
        for id in install::installed(&self.home) {
            let plugin = match install::admitted(&self.home, &id) {
                Ok(plugin) => plugin,
                Err(refused) => {
                    if hooks_for_unchecked(&self.home, &id, name) {
                        note(&self.home, &id, &format!("{name}: not run: {refused}"));
                    }
                    continue;
                }
            };
            for hook in hooks_for(&plugin.manifest.here().events, name) {
                let mut env = install::plugin_env(&self.home, &id, &plugin.root);
                env.extend(event.scope.env.clone());
                env.push(("HERDR_PLUGIN_EVENT".into(), event.dotted.into()));
                env.push(("HERDR_PLUGIN_EVENT_JSON".into(), payload.to_string().into()));
                env.push((
                    "HERDR_PLUGIN_CONTEXT_JSON".into(),
                    context.to_string().into(),
                ));
                {
                    let mut waiting = self.waiting.lock().unwrap();
                    let queued = waiting.entry(id.clone()).or_default();
                    if *queued >= WAITING_PER_PLUGIN {
                        drop(waiting);
                        note(
                            &self.home,
                            &id,
                            &format!("{name}: not run: too many hooks waiting"),
                        );
                        continue;
                    }
                    *queued += 1;
                }
                let (gate, home, id) = (self.hooks.clone(), self.home.clone(), id.clone());
                let (root, command) = (plugin.root.clone(), hook.command.clone());
                let waiting = self.waiting.clone();
                tokio::spawn(async move {
                    let turn = gate.acquire_owned().await;
                    if let Some(queued) = waiting.lock().unwrap().get_mut(&id) {
                        *queued -= 1;
                    }
                    let Ok(_turn) = turn else {
                        return;
                    };
                    // Asked again at its turn: a plugin removed, or whose manifest changed,
                    // while this waited does not run it.
                    let still = install::admitted(&home, &id).is_ok_and(|p| {
                        p.root == root
                            && hooks_for(&p.manifest.here().events, name)
                                .iter()
                                .any(|h| h.command == command)
                    });
                    if !still {
                        note(&home, &id, &format!("{name}: not run: no longer agreed to"));
                        return;
                    }
                    let log = home.state_of(&id).join("host.log");
                    match spawn(&root, &command, env, &log) {
                        Ok(child) => {
                            let _ = tokio::task::spawn_blocking(move || {
                                wait_bounded(child, HANDLER_LIMIT)
                            })
                            .await;
                        }
                        Err(e) => note(&home, &id, &format!("{name}: {command:?}: {e}")),
                    }
                });
            }
        }
    }

    /// Start every plugin's `[[startup]]` commands, long-lived, once per host; the ones a
    /// killed host left are ended first, so none runs twice.
    fn run_startups(&self) {
        end_startups(&self.home, None);
        let scope = focused_scope(&self.state.lock().unwrap());
        let context = invoked(&scope.context, "startup", "plugin.startup");
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
                env.push(("HERDR_PLUGIN_EVENT".into(), "startup".into()));
                env.push((
                    "HERDR_PLUGIN_CONTEXT_JSON".into(),
                    context.to_string().into(),
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

    /// Take `terminal` as what is shown, when it is listed, and tell the focus events that
    /// follow from it. `reported` says it came from what a person sees (the interface, or a
    /// `Focus` the daemon passed on), not only from a plugin's own call.
    pub(super) fn set_focus(&self, terminal: u32, reported: bool) -> bool {
        let events = {
            let mut state = self.state.lock().unwrap();
            if !state.now.rows.iter().any(|r| r.terminal == terminal) {
                return false;
            }
            state.focused = Some(terminal);
            state.reported |= reported;
            state.focus_events()
        };
        for event in events {
            let _ = self.events.send(Arc::new(event));
        }
        true
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
/// let settle for [`COALESCE`] and read as one. A `Focus` the daemon passes on (`qoop focus`,
/// a notch) is what every interface now shows, and is taken as the focus. `Err` when the daemon
/// closed the link.
async fn changed(host: &Host, link: &mut Link) -> std::io::Result<()> {
    let poll = Instant::now() + POLL;
    loop {
        let Some(events) = link.next_events(Some(poll)).await? else {
            return Ok(());
        };
        for event in &events {
            if let EventMessage::Focus { terminal, .. } = event {
                host.set_focus(*terminal, true);
            }
        }
        if events.iter().any(moves_rows) {
            tokio::time::sleep(COALESCE).await;
            // What came meanwhile is read with it, for one more window at most, so a daemon
            // that never goes quiet still gets listed.
            let until = Instant::now() + COALESCE;
            let soon = || Some((Instant::now() + Duration::from_millis(1)).min(until));
            while Instant::now() < until && link.next_events(soon()).await?.is_some() {}
            return Ok(());
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
                if install::installed(&host.home).is_empty()
                    || changed(host, &mut link).await.is_err()
                {
                    break;
                }
            }
            // The daemon went: its next listing, which may come before it holds every tab
            // again, prunes no names.
            host.state.lock().unwrap().prune_ready = false;
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

/// The last screen row that counts as written: the last with text, or the cursor's row when it
/// is lower (a prompt waiting at an empty line). `None` for a screen with neither.
fn screen_end(screen: &[String], cursor_row: usize) -> Option<usize> {
    let last = screen.iter().rposition(|r| !r.trim().is_empty());
    match last {
        Some(row) => Some(row.max(cursor_row).min(screen.len().saturating_sub(1))),
        None if screen.is_empty() => None,
        None => Some(cursor_row.min(screen.len() - 1)),
    }
}

/// herdr's `recent` read: the last `n` rows written, history then the screen up to
/// [`screen_end`]; on the alternate screen only its last `n` rows. Blank rows at the end go.
fn recent_rows(
    history: Vec<String>,
    screen: &[String],
    cursor_row: usize,
    alt: bool,
    n: usize,
) -> Vec<String> {
    let mut rows = if alt {
        screen.to_vec()
    } else {
        let end = screen_end(screen, cursor_row).map_or(0, |end| end + 1);
        let mut rows = history;
        rows.extend_from_slice(&screen[..end]);
        rows
    };
    let mut rows = rows.split_off(rows.len().saturating_sub(n));
    while rows.last().is_some_and(|r| r.trim().is_empty()) {
        rows.pop();
    }
    rows
}

/// Rows a program wrote past the right edge joined back into the lines it wrote, as herdr's
/// `recent_unwrapped` reads them: a row that fills every column runs on into the next.
///
/// ponytail: a full row is taken as wrapped, since neither the screen nor the history says
/// which rows wrapped. A line exactly as wide as the pane joins the next, and a wrap at a
/// space is missed; a wrap flag per row from the engine is the upgrade.
fn unwrapped(rows: Vec<String>, cols: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut runs_on = false;
    for row in rows {
        // A char is a column here: the engine keeps a wide character's spacer.
        let full = cols > 0 && row.chars().count() >= cols;
        match out.last_mut() {
            Some(last) if runs_on => last.push_str(&row),
            _ => out.push(row),
        }
        runs_on = full;
    }
    out
}

/// A line answered under `id` as the request wrote it.
fn under(mut line: Value, id: &Value) -> Value {
    line["id"] = id.clone();
    line
}

/// The key a request carries the asking process under: set here from that process alone.
pub(super) const PEER: &str = "qoop_peer";

/// The plugin identity of the process at the other end: the pane, plugin and plugin pane
/// (entrypoint) its environment names, as herdr set them for it (a plugin pane's program, a
/// hook, an action, or the shim one of them ran).
fn peer_of(pid: u32) -> Value {
    let [pane, plugin, entrypoint] = env_of_process(
        pid,
        [
            "HERDR_PANE_ID",
            "HERDR_PLUGIN_ID",
            "HERDR_PLUGIN_ENTRYPOINT_ID",
        ],
    )
    .unwrap_or_default();
    translate::compact(json!({"pane": pane, "plugin": plugin, "entrypoint": entrypoint}))
}

/// `names` as the starting environment of process `pid` holds them, or `None` when it cannot
/// be read or `pid` became another process while it was read (its start time moved).
///
/// What this proves is courtesy, not a boundary: any process of this user can start a program
/// with these variables set (ADR-018).
pub(super) fn env_of_process<const N: usize>(
    pid: u32,
    names: [&str; N],
) -> Option<[Option<String>; N]> {
    let pid = qoop_pty::Pid(pid as i32);
    let started = qoop_pty::sys::start_time_of(pid)?;
    let values = names.map(|name| qoop_pty::sys::env_of(pid, name));
    (qoop_pty::sys::start_time_of(pid)? == started).then_some(values)
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
    // Who is asking, from the asking process's own environment: what a person's key in a
    // plugin pane grants ([`PEER`]) is never taken from what a request says.
    let peer = qoop_runtime::daemon::peer_pid(&stream).map(peer_of);
    let (rd, mut wr) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(rd).lines();
    let mut early = None;
    loop {
        let line = match early.take() {
            Some(line) => line,
            None => match lines.next_line().await {
                Ok(Some(line)) => line,
                _ => return,
            },
        };
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
        let mut params = request.get("params").cloned().unwrap_or(json!({}));
        if let Value::Object(fields) = &mut params {
            fields.remove(PEER);
            if let Some(peer) = &peer {
                fields.insert(PEER.into(), peer.clone());
            }
        }
        if method == "events.subscribe" {
            stream_events(&host, &id, &params, wr).await;
            return;
        }
        let answer = host.answer("", method, &params);
        let waits = method == "agent.prompt" && params.get("wait").is_some_and(Value::is_object);
        let answer = if waits {
            // A prompt waiting for its turn ends when its peer leaves (by then the text is
            // typed, or `qoop prompt` is on its way and finishes it). A peer that only shuts
            // its write side reads as gone too.
            tokio::pin!(answer);
            loop {
                tokio::select! {
                    a = &mut answer => break a,
                    next = lines.next_line(), if early.is_none() => match next {
                        Ok(Some(line)) => early = Some(line),
                        _ => return,
                    },
                }
            }
        } else {
            answer.await
        };
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

/// One subscription: a dotted type, bound to a pane for the per-pane ones, and for
/// `pane.agent_status_changed` optionally to one status.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    kind: &'static str,
    pane: Option<String>,
    status: Option<String>,
}

impl Wanted {
    fn hears(&self, event: &Event) -> bool {
        self.kind == event.dotted
            && self
                .pane
                .as_ref()
                .is_none_or(|p| Some(p) == event.pane.as_ref())
            && self
                .status
                .as_ref()
                .is_none_or(|s| event.data["agent_status"] == s.as_str())
    }
}

/// What a subscription asks for.
fn subscriptions(params: &Value) -> Result<Vec<Wanted>, String> {
    let list = params["subscriptions"]
        .as_array()
        .ok_or("invalid request: missing field `subscriptions`")?;
    list.iter()
        .map(|s| {
            let asked = s["type"].as_str().unwrap_or_default();
            let Some(kind) = translate::SUBSCRIPTIONS
                .iter()
                .map(|(dotted, _)| *dotted)
                .find(|dotted| *dotted == asked)
            else {
                return Err(format!("invalid request: unknown variant `{asked}`"));
            };
            let pane = s["pane_id"].as_str().map(str::to_string);
            if PER_PANE.contains(&kind) && pane.is_none() {
                return Err(format!("invalid request: {kind} needs a `pane_id`"));
            }
            let status = s["agent_status"].as_str().map(str::to_string);
            if let Some(status) = &status
                && !["idle", "working", "blocked", "done", "unknown"].contains(&status.as_str())
            {
                return Err(format!("invalid request: unknown variant `{status}`"));
            }
            Ok(Wanted { kind, pane, status })
        })
        .collect()
}

/// The lines a subscriber is sent before any event: for a status it asked of a pane that has
/// it already, one event now, as herdr sends it. `Err` with the pane id when a pane it names
/// is not there.
fn first_lines(host: &Host, wanted: &[Wanted]) -> Result<Vec<Value>, String> {
    let state = host.state.lock().unwrap();
    let mut out = Vec::new();
    for w in wanted {
        let Some(pane) = &w.pane else {
            continue;
        };
        let row = terminal_of(pane)
            .and_then(|t| state.now.rows.iter().find(|r| r.terminal == t))
            .ok_or_else(|| pane.clone())?;
        if w.kind == "pane.agent_status_changed"
            && w.status.as_deref() == Some(translate::agent_status(row))
        {
            let event = Event::new(w.kind, Some(pane.clone()), status_data(pane, row));
            out.extend(event.for_subscriber());
        }
    }
    Ok(out)
}

/// What herdr says when a subscriber fell further behind than the history it keeps.
const EVENTS_LOST: &str =
    "event subscription fell behind retained history; resubscribe and resync with session.snapshot";

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
    if wanted.iter().any(|w| w.pane.is_some()) {
        let _ = host.refresh().await;
    }
    let first = match first_lines(host, &wanted) {
        Ok(first) => first,
        Err(pane) => {
            let refused =
                translate::refuse("", "pane_not_found", &format!("pane {pane} not found"));
            let _ = send(&mut wr, &under(refused, id)).await;
            return;
        }
    };
    let mut rx = host.events.subscribe();
    let ack = translate::reply("", json!({"type": "subscription_started"}));
    if send(&mut wr, &under(ack, id)).await.is_err() {
        return;
    }
    for line in first {
        if send(&mut wr, &line).await.is_err() {
            return;
        }
    }
    loop {
        let line = match rx.recv().await {
            Ok(event) => match event.for_subscriber() {
                Some(line) if wanted.iter().any(|w| w.hears(&event)) => line,
                _ => continue,
            },
            // Past the history a subscriber may lag by, the stream ends, as herdr's does.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let lost = translate::refuse("", "events_lost", EVENTS_LOST);
                let _ = send(&mut wr, &under(lost, id)).await;
                return;
            }
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

    #[test]
    fn a_recent_read_is_the_last_rows_written_screen_included() {
        let s = |v: &[&str]| v.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        // After `seq 1 60` on a short screen: 57 to 60 and the prompt are on screen.
        let screen = s(&["57", "58", "59", "60", "us@us-2 repo %", "", ""]);
        let history = s(&["55", "56"]);
        assert_eq!(
            recent_rows(Vec::new(), &screen, 4, false, 5),
            s(&["57", "58", "59", "60", "us@us-2 repo %"]),
            "herdr: 57 58 59 60 and the prompt"
        );
        assert_eq!(
            recent_rows(history, &screen, 4, false, 7),
            s(&["55", "56", "57", "58", "59", "60", "us@us-2 repo %"]),
            "the history makes up what the screen does not hold"
        );
        assert_eq!(screen_end(&screen, 4), Some(4));
        assert_eq!(
            screen_end(&screen, 6),
            Some(6),
            "a cursor below the text counts"
        );
        // Output that still fits the screen is read, not an empty answer.
        let short = s(&["fake agent working", "fake agent done", ""]);
        assert_eq!(
            recent_rows(Vec::new(), &short, 1, false, 80),
            s(&["fake agent working", "fake agent done"])
        );
        // The alternate screen: its own last rows, no history.
        let alt = s(&["top", "middle", "bottom", ""]);
        assert_eq!(recent_rows(Vec::new(), &alt, 0, true, 2), s(&["bottom"]));
    }

    #[test]
    fn an_unwrapped_read_joins_the_rows_a_line_ran_over() {
        let s = |rows: &[&str]| rows.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        // Four columns: `abcdefghij` wrote three rows, `xy` one.
        assert_eq!(
            unwrapped(s(&["abcd", "efgh", "ij", "xy"]), 4),
            s(&["abcdefghij", "xy"])
        );
        assert_eq!(unwrapped(s(&["ab", "cd"]), 4), s(&["ab", "cd"]));
        assert_eq!(unwrapped(s(&["abcd"]), 4), s(&["abcd"]), "full at the end");
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
            [
                "tab.created",
                "pane.created",
                "pane.agent_detected",
                "layout.updated"
            ],
            "a pane listed with its agent tells it right after the pane, as herdr's later \
             detection does"
        );
        assert_eq!(events[2].data["agent"], "claude");
        assert_eq!(events[1].data["pane"]["pane_id"], "w1:p2");
        assert_eq!(events[0].data["tab"]["tab_id"], "w1:t2");
        let (hook, payload) = events[1].for_hook().expect("a hook name");
        assert_eq!(hook, "pane_created");
        assert_eq!(payload["data"]["type"], "pane_created");

        let events = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        assert_eq!(
            names(&events),
            ["pane.exited", "tab.closed"],
            "gone on its own is exited, never closed too"
        );
        assert_eq!(
            events[0].data,
            json!({"pane_id": "w1:p2", "workspace_id": "w1"})
        );
        assert_eq!(events[1].data["tab_id"], "w1:t2");

        // Closed through the host: closed, and only that.
        state.update(
            vec![space(1, "api")],
            vec![row(1, 1, json!({})), row(3, 1, json!({}))],
            &[],
        );
        state.closing.insert(3);
        let events = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        assert_eq!(names(&events), ["pane.closed", "tab.closed"]);
    }

    #[test]
    fn a_split_is_told_once_and_never_as_a_passing_tab() {
        let mut state = primed();
        // The new terminal is made hidden, listed first as a tab of its own, then split.
        state.hidden.insert(2);
        let rows = || vec![row(1, 1, json!({})), row(2, 1, json!({}))];
        assert!(state.update(vec![space(1, "api")], rows(), &[]).is_empty());
        assert!(
            state
                .update(vec![space(1, "api")], rows(), &[split(1, 2)])
                .is_empty()
        );
        assert_eq!(state.now.rows.len(), 2, "reads still see it");
        state.hidden.remove(&2);
        let events = state.update(vec![space(1, "api")], rows(), &[split(1, 2)]);
        assert_eq!(names(&events), ["pane.created", "layout.updated"]);
        assert_eq!(events[0].data["pane"]["tab_id"], "w1:t1");
    }

    #[test]
    fn a_tab_pane_is_told_its_spaces_focused_pane_and_folder() {
        let mut state = State::default();
        state.update(
            vec![space(1, "a"), space(2, "b")],
            vec![
                row(1, 1, json!({"cwd": "/src/a"})),
                row(2, 2, json!({"cwd": "/src/b"})),
                row(3, 2, json!({"cwd": "/src/b/sub"})),
            ],
            &[],
        );
        state.focused = Some(3);
        assert_eq!(state.space_pane(2), Some(3), "the one shown, in that space");
        assert_eq!(state.space_pane(1), Some(1), "else the space's first");
        let context = scope(&state.now, 2, state.space_pane(2)).context;
        assert_eq!(context["focused_pane_cwd"], "/src/b/sub");
        assert_eq!(context["workspace_cwd"], "/src/b/sub");
        assert_eq!(context["focused_pane_id"], "w2:p3");
    }

    #[test]
    fn plugin_list_carries_the_manifest_as_herdr_lists_it() {
        let m = manifest::Manifest::parse(&format!(
            "{}description = \"Notes\"\nplatforms = [\"macos\"]\n\
             [[actions]]\nid = \"go\"\ntitle = \"Go\"\ndescription = \"Go now\"\ncommand = [\"g\"]\n\
             [[events]]\non = \"pane.closed\"\ncommand = [\"e\"]\n\
             [[panes]]\nid = \"v\"\ntitle = \"V\"\nplacement = \"popup\"\nwidth = \"60%\"\ncommand = [\"v\"]\n\
             [[link_handlers]]\nid = \"md\"\ntitle = \"Markdown\"\npattern = \"[.]md$\"\naction = \"go\"\n",
            manifest::head("t.n")
        ))
        .expect("parses");
        let mut info = json!({"plugin_id": "t.n"});
        described(&mut info, &m);
        assert_eq!(info["description"], "Notes");
        assert_eq!(info["min_herdr_version"], "0.9.0");
        assert_eq!(info["platforms"], json!(["macos"]));
        assert_eq!(info["actions"][0]["description"], "Go now");
        assert_eq!(
            info["events"][0],
            json!({"on": "pane.closed", "command": ["e"]})
        );
        assert_eq!(info["panes"][0]["width"], "60%");
        assert_eq!(
            info["link_handlers"],
            json!([{"id": "md", "title": "Markdown", "pattern": "[.]md$", "action": "go"}])
        );
        assert!(info.get("build").is_none(), "empty lists are left out");
    }

    #[test]
    fn names_plugins_give_are_kept_across_a_restart_and_go_with_their_terminal() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        assert_eq!(load_names(&home), translate::Names::default(), "none yet");
        let mut names = translate::Names::default();
        names.panes.insert(3, "logs".into());
        names.agents.insert(4, "reviewer".into());
        save_names(&home, &names);
        let host = Host::new(home.clone());
        assert_eq!(
            host.state.lock().unwrap().names,
            names,
            "a new host reads them back"
        );
        let mut state = State {
            names: names.clone(),
            ..State::default()
        };
        // The first listing after a start (or a daemon that came back) may not hold every tab
        // yet: nothing is pruned on it.
        state.update(vec![space(1, "a")], vec![row(3, 1, json!({}))], &[]);
        assert_eq!(state.names, names, "nothing pruned on the first listing");
        // A sleeping tab keeps its names for its wake.
        let mut sleeping = space(1, "a");
        sleeping.sleeping = vec![
            serde_json::from_value(json!({"terminal": 4, "title": null, "agent": null,
                                          "cwd": "/tmp"}))
            .expect("a sleeping tab"),
        ];
        state.update(vec![sleeping], vec![row(3, 1, json!({}))], &[]);
        assert_eq!(state.names, names, "terminal 4 sleeps");
        assert!(!state.names_changed);
        state.update(vec![space(1, "a")], vec![row(3, 1, json!({}))], &[]);
        assert_eq!(state.names.panes.get(&3).map(String::as_str), Some("logs"));
        assert!(
            state.names.agents.is_empty(),
            "terminal 4 is gone, and its name"
        );
        assert!(state.names_changed, "to be written");
    }

    #[test]
    #[ignore = "a program whose environment the tests read"]
    fn a_sleeper_to_read() {
        std::thread::sleep(Duration::from_secs(5));
    }

    #[test]
    fn the_asker_is_known_by_its_own_environment() {
        // This test binary, running only the sleeper below: a program of ours, whose start
        // environment can be read (a system one's cannot).
        let mut child = std::process::Command::new(std::env::current_exe().expect("exe"))
            .args([
                "--ignored",
                "--exact",
                "plugin::host::tests::a_sleeper_to_read",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env("HERDR_PANE_ID", "w1:p9")
            .env("HERDR_PLUGIN_ID", "t.rev")
            .env("HERDR_PLUGIN_ENTRYPOINT_ID", "pane")
            .spawn()
            .expect("sleep");
        std::thread::sleep(Duration::from_millis(300));
        let peer = peer_of(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            peer,
            json!({"pane": "w1:p9", "plugin": "t.rev", "entrypoint": "pane"})
        );
    }

    #[test]
    fn a_popup_is_no_pane_to_a_plugin() {
        let mut state = primed();
        state.popups.insert(
            2,
            Popup {
                previous: Some(1),
                plugin: "t.bar".into(),
                popup: true,
            },
        );
        state.focused = Some(2);
        let rows = vec![row(1, 1, json!({})), row(2, 1, json!({}))];
        let events = state.update(vec![space(1, "api")], rows, &[split(1, 2)]);
        let made = |events: &[Event]| {
            names(events)
                .into_iter()
                .filter(|n| {
                    matches!(
                        *n,
                        "pane.created" | "pane.closed" | "pane.exited" | "layout.updated"
                    )
                })
                .count()
        };
        assert_eq!(made(&events), 0, "{:?}", names(&events));
        let panes = state.view().panes();
        assert_eq!(panes.len(), 1, "only the pane behind it");
        assert_eq!(panes[0]["pane_id"], "w1:p1");
        assert_eq!(panes[0]["focused"], true, "the focus behind the popup");
        // Closed: nothing is told either.
        state.popups.clear();
        let events = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        assert_eq!(made(&events), 0, "{:?}", names(&events));
    }

    #[test]
    fn a_tab_closed_by_the_host_is_told_as_tab_closed_alone() {
        let mut state = primed();
        let rows = vec![
            row(1, 1, json!({})),
            row(2, 1, json!({"name": "logs"})),
            row(3, 1, json!({})),
        ];
        state.update(vec![space(1, "api")], rows, &[split(2, 3)]);
        state.closing.extend([2, 3]);
        state.closing_tab.extend([2, 3]);
        // The first pane goes first: no rename, no layout, no pane event on the way.
        let events = state.update(
            vec![space(1, "api")],
            vec![row(1, 1, json!({})), row(3, 1, json!({}))],
            &[],
        );
        assert!(events.is_empty(), "{:?}", names(&events));
        let events = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        assert_eq!(names(&events), ["tab.closed"]);
    }

    #[test]
    fn an_agent_found_is_told_before_any_status() {
        let mut state = primed();
        let codex = |s: &str| row(1, 1, json!({"agent": true, "program": "codex", "state": s}));
        let events = state.update(vec![space(1, "api")], vec![codex("unknown")], &[]);
        let pane_events: Vec<&str> = names(&events)
            .into_iter()
            .filter(|n| n.starts_with("pane."))
            .collect();
        assert_eq!(
            pane_events,
            ["pane.agent_detected"],
            "no leading unknown status"
        );
        assert_eq!(
            state.seqs[&1], 1,
            "herdr's state_change_seq starts at the agent"
        );
        let events = state.update(vec![space(1, "api")], vec![codex("idle")], &[]);
        assert_eq!(names(&events), ["pane.agent_status_changed"]);
        assert_eq!(state.seqs[&1], 2);
        assert_eq!(events[0].data["agent_status"], "idle");
        // One count across every pane, as herdr keeps it: a second agent's first status is
        // the next number, and the first pane keeps the number of its own last change.
        let claude = row(
            2,
            1,
            json!({"agent": true, "program": "claude", "state": "idle"}),
        );
        state.update(vec![space(1, "api")], vec![codex("idle"), claude], &[]);
        assert_eq!((state.seqs[&1], state.seqs[&2]), (2, 3));
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
        assert_eq!(
            names(&events),
            ["pane.exited", "layout.updated"],
            "a tab that had two panes tells its new layout"
        );
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
    fn a_token_change_is_told_and_expiry_tells_it_too() {
        let mut state = State::default();
        state.update(vec![space(1, "a")], vec![row(4, 1, json!({}))], &[]);
        let now = Instant::now();
        let pane = |tokens: Value, ttl: Option<u64>| {
            let mut p = json!({"pane_id": "w1:p4", "source": "t.sb", "tokens": tokens});
            if let Some(ms) = ttl {
                p["ttl_ms"] = json!(ms);
            }
            p
        };
        let report = |state: &mut State, method, params: Value| {
            Host::report_tokens(state, method, &params, now).expect("accepted")
        };
        let before = state.revisions.get(&4).copied().unwrap_or(0);
        let events = report(
            &mut state,
            "pane.report_metadata",
            pane(json!({"a": "1"}), Some(1000)),
        );
        assert_eq!(names(&events), ["pane.updated"]);
        assert_eq!(events[0].data["pane"]["tokens"], json!({"a": "1"}));
        assert_eq!(events[0].pane.as_deref(), Some("w1:p4"));
        assert_eq!(state.revisions[&4], before + 1);
        let line = events[0].for_subscriber().expect("subscribers hear it");
        assert_eq!(line["event"], "pane_updated");
        assert!(NOT_HOOKABLE.contains(&events[0].dotted), "never a hook");
        assert!(
            report(
                &mut state,
                "pane.report_metadata",
                pane(json!({"a": "1"}), Some(1000))
            )
            .is_empty(),
            "no change, no event"
        );

        let space_report = json!({"workspace_id": "w1", "source": "t.sb", "tokens": {"k": "v"}});
        let events = report(&mut state, "workspace.report_metadata", space_report);
        assert_eq!(names(&events), ["workspace.metadata_updated"]);
        assert_eq!(events[0].data["workspace"]["tokens"], json!({"k": "v"}));
        assert!(NOT_HOOKABLE.contains(&events[0].dotted));

        // The poll removes what expired, and says so as a report would.
        let revision = state.revisions[&4];
        let events = state.expire_tokens(now + Duration::from_secs(2));
        assert_eq!(names(&events), ["pane.updated"]);
        assert!(events[0].data["pane"].get("tokens").is_none(), "gone");
        assert_eq!(state.revisions[&4], revision + 1);
        assert!(state.expire_tokens(now + Duration::from_secs(3)).is_empty());
    }

    #[test]
    fn a_popup_gives_focus_back_only_while_it_has_it() {
        let rows = |n: &[u32]| n.iter().map(|t| row(*t, 1, json!({}))).collect::<Vec<_>>();
        for (focused, want) in [(3, Some(1)), (2, Some(2))] {
            let mut state = State::default();
            state.update(vec![space(1, "a")], rows(&[1, 2, 3]), &[]);
            state.popups.insert(
                3,
                Popup {
                    previous: Some(1),
                    plugin: "t.bar".into(),
                    popup: true,
                },
            );
            state.focused = Some(focused);
            state.update(vec![space(1, "a")], rows(&[1, 2]), &[]);
            assert_eq!(state.focused, want, "focused {focused} when the popup went");
            assert_eq!(
                state.restore,
                if focused == 3 { vec![1] } else { vec![] },
                "a plugin that focused elsewhere first keeps it"
            );
        }
    }

    #[test]
    fn a_focus_move_tells_space_tab_and_pane_in_that_order() {
        let mut state = State::default();
        state.update(
            vec![space(1, "a"), space(2, "b")],
            vec![row(1, 1, json!({})), row(2, 2, json!({}))],
            &[],
        );
        state.focused = Some(2);
        let told = state.focus_events();
        assert_eq!(
            names(&told),
            ["workspace.focused", "tab.focused", "pane.focused"]
        );
        assert_eq!(told[0].data, json!({"workspace_id": "w2"}));
        assert_eq!(
            told[1].data,
            json!({"tab_id": "w2:t2", "workspace_id": "w2"})
        );
        assert_eq!(
            told[2].data,
            json!({"pane_id": "w2:p2", "workspace_id": "w2"})
        );
        assert!(state.focus_events().is_empty(), "told once");
        let (hook, _) = told[2].for_hook().expect("hookable");
        assert_eq!(hook, "pane_focused");
    }

    #[test]
    fn a_tab_is_renamed_by_its_name_only_and_a_split_is_a_layout() {
        let mut state = primed();
        // A program that changes is not a rename: an unnamed tab is labelled by its number.
        let events = state.update(
            vec![space(1, "api")],
            vec![row(1, 1, json!({"program": "vim"}))],
            &[],
        );
        assert!(names(&events).is_empty(), "{:?}", names(&events));
        assert_eq!(state.view().tab_infos()[0]["label"], "1");
        let events = state.update(
            vec![space(1, "api")],
            vec![row(1, 1, json!({"name": "logs2"}))],
            &[],
        );
        assert_eq!(names(&events), ["tab.renamed"], "its own name changed");
        assert_eq!(events[0].data["label"], "logs2");
        let events = state.update(
            vec![space(1, "api")],
            vec![row(1, 1, json!({"name": "logs2"})), row(7, 1, json!({}))],
            &[split(1, 7)],
        );
        assert_eq!(names(&events), ["pane.created", "layout.updated"]);
        assert_eq!(events[1].data["layout"]["panes"][1]["pane_id"], "w1:p7");
        assert!(events[1].for_hook().is_some() && NOT_HOOKABLE.contains(&events[1].dotted));
    }

    #[test]
    fn an_agent_arriving_changing_title_and_leaving() {
        let mut state = primed();
        let claude = |extra: Value| {
            let mut fields = json!({"agent": true, "program": "claude", "state": "idle"});
            if let (Value::Object(f), Value::Object(e)) = (&mut fields, extra) {
                f.extend(e);
            }
            row(1, 1, fields)
        };
        let events = state.update(vec![space(1, "api")], vec![claude(json!({}))], &[]);
        // The tab is named after its program here, so it is renamed too; the pane events:
        let events: Vec<Event> = events
            .into_iter()
            .filter(|e| e.dotted.starts_with("pane."))
            .collect();
        assert_eq!(
            names(&events),
            ["pane.agent_detected", "pane.agent_status_changed"]
        );
        assert_eq!(events[0].data["agent"], "claude");
        assert!(
            events[1].data.get("state_labels").is_none(),
            "empty is left out"
        );
        let events = state.update(
            vec![space(1, "api")],
            vec![claude(json!({"agent_title": "Fix login"}))],
            &[],
        );
        assert_eq!(
            names(&events),
            ["pane.agent_status_changed"],
            "a title is presentation"
        );
        assert_eq!(events[0].data["title"], "Fix login");
        let events = state.update(vec![space(1, "api")], vec![row(1, 1, json!({}))], &[]);
        let left = events
            .iter()
            .find(|e| e.dotted == "pane.agent_detected")
            .expect("left");
        assert_eq!(
            left.data,
            json!({"pane_id": "w1:p1", "workspace_id": "w1", "agent": "claude",
                   "released": true, "final_status": "idle"})
        );
    }

    #[tokio::test]
    async fn a_plugin_pane_is_checked_as_herdr_checks_it() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        let folder = dir.path().join("p");
        std::fs::create_dir_all(&folder).expect("mkdir");
        let text = format!(
            "{}[[panes]]\nid = \"v\"\ntitle = \"V\"\nplacement = \"popup\"\ncommand = [\"v\"]\n",
            manifest::head("t.p")
        );
        std::fs::write(folder.join(manifest::FILE), &text).expect("manifest");
        install::link_for_tests(&home, &folder);
        let host = Host::new(home);
        let refused = |params: Value| {
            let host = host.clone();
            async move { host.open_pane(&params).await.expect_err("refused") }
        };
        let base = json!({"plugin_id": "t.p", "entrypoint": "v"});
        let with = |extra: Value| {
            let mut p = base.clone();
            if let (Value::Object(p), Value::Object(e)) = (&mut p, extra) {
                p.extend(e);
            }
            p
        };
        assert_eq!(
            refused(with(json!({"entrypoint": "nope"}))).await,
            (
                "plugin_pane_not_found",
                "plugin pane entrypoint 'nope' not found".into()
            )
        );
        assert_eq!(
            refused(with(json!({"placement": "split", "width": 10}))).await,
            (
                "invalid_params",
                "width and height are only supported when placement is popup".into()
            )
        );
        assert_eq!(
            refused(with(json!({"target_pane_id": "w1:p1"}))).await,
            (
                "invalid_params",
                "overlay and popup plugin panes target the active pane".into()
            )
        );
        assert_eq!(
            refused(with(json!({"placement": "split", "workspace_id": "w1"}))).await,
            (
                "invalid_params",
                "split and zoomed plugin panes target an existing pane; use target_pane_id".into()
            )
        );
        assert_eq!(
            refused(with(json!({"placement": "tab", "direction": "down"}))).await,
            (
                "invalid_params",
                "tab plugin panes support workspace_id but not target_pane_id or direction".into()
            )
        );
        let open = |popup| {
            host.state.lock().unwrap().popups.insert(
                5,
                Popup {
                    previous: None,
                    plugin: "t.p".into(),
                    popup,
                },
            )
        };
        open(false);
        assert_ne!(
            refused(base.clone()).await.0,
            "ui_busy",
            "an open overlay does not block a popup"
        );
        open(true);
        assert_eq!(
            refused(base.clone()).await,
            ("ui_busy", "a popup pane is already open".into())
        );
        assert_eq!(
            refused(json!({"plugin_id": "nope", "entrypoint": "v"})).await,
            ("plugin_not_found", "plugin not found".into())
        );
    }

    #[test]
    fn an_action_named_without_its_plugin_is_found_once_or_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        for (id, action) in [("t.a", "go"), ("t.b", "go"), ("t.b", "only")] {
            let folder = dir.path().join(id);
            std::fs::create_dir_all(&folder).expect("mkdir");
            let manifest_at = folder.join(manifest::FILE);
            let mut text =
                std::fs::read_to_string(&manifest_at).unwrap_or_else(|_| manifest::head(id));
            text +=
                &format!("[[actions]]\nid = \"{action}\"\ntitle = \"T\"\ncommand = [\"true\"]\n");
            std::fs::write(&manifest_at, text).expect("manifest");
        }
        for id in ["t.a", "t.b"] {
            install::link_for_tests(&home, &dir.path().join(id));
        }
        let host = Host::new(home);
        let ask = |params: Value| host.invoke_param(&params).expect_err("refused");
        assert_eq!(
            ask(json!({"action_id": "go"})),
            (
                "ambiguous_plugin_action",
                "plugin action id matches more than one action; include plugin_id".into()
            )
        );
        assert_eq!(
            ask(json!({"action_id": "nothing"})).0,
            "plugin_action_not_found"
        );
        assert_eq!(
            ask(json!({"plugin_id": "t.a", "action_id": "only"})).0,
            "plugin_action_not_found"
        );
        assert_eq!(
            ask(json!({"plugin_id": "t.zz", "action_id": "go"})),
            ("plugin_not_found", "plugin not found".into())
        );
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
        let mut both: Vec<&str> = SERVED.iter().chain(WRITES).copied().collect();
        both.sort_unstable();
        let mut methods = translate::METHODS.to_vec();
        methods.sort_unstable();
        assert_eq!(both, methods);
        assert!(SERVED.iter().all(|m| !WRITES.contains(m)));
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
        assert_eq!(ping["result"]["type"], "pong");
        assert_eq!(ping["result"]["version"], "0.9.5");
        assert_eq!(ping["result"]["protocol"], 22);
        assert_eq!(
            ping["result"]["capabilities"]["health_check"], true,
            "herdr's capabilities, as this host has them"
        );
        assert_eq!(ping["result"]["capabilities"]["live_handoff"], false);
    }

    #[test]
    fn subscriptions_take_only_herdrs_types() {
        let ok = subscriptions(&json!({"subscriptions": [
            {"type": "pane.created"}, {"type": "pane.agent_status_changed", "pane_id": "w1:p2"}
        ]}))
        .expect("ok");
        assert_eq!(ok[1].kind, "pane.agent_status_changed");
        assert_eq!(ok[1].pane.as_deref(), Some("w1:p2"));
        let filtered = subscriptions(&json!({"subscriptions": [
            {"type": "pane.agent_status_changed", "pane_id": "w1:p2", "agent_status": "done"}
        ]}))
        .expect("ok");
        assert_eq!(filtered[0].status.as_deref(), Some("done"));
        assert!(
            subscriptions(&json!({"subscriptions": [
                {"type": "pane.agent_status_changed", "pane_id": "w1:p2", "agent_status": "x"}
            ]}))
            .is_err()
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
        let m = manifest::Manifest::parse(&format!(
            "{}\
             [[events]]\non = \"pane.closed\"\ncommand = [\"a\"]\n\
             [[events]]\non = \"pane.agent_status_changed\"\ncommand = [\"b\"]\n\
             [[events]]\non = \"tab.closed\"\ncommand = [\"c\"]\n",
            manifest::head("h")
        ))
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

    #[tokio::test]
    async fn a_waiting_hook_is_asked_again_and_the_queue_is_bounded() {
        let dir = tempfile::tempdir().expect("dir");
        let home = test_home(dir.path());
        let folder = dir.path().join("p");
        std::fs::create_dir_all(&folder).expect("mkdir");
        let manifest_at = folder.join(manifest::FILE);
        let text = format!(
            "{}[[events]]\non = \"pane.closed\"\ncommand = [\"sh\", \"-c\", \"touch ran\"]\n",
            manifest::head("t.q")
        );
        std::fs::write(&manifest_at, &text).expect("manifest");
        install::link_for_tests(&home, &folder);
        let host = Host::new(home.clone());
        // Every turn taken, so what is asked for waits.
        let turns = host
            .hooks
            .clone()
            .acquire_many_owned(HANDLERS_AT_ONCE as u32)
            .await
            .expect("turns");
        let closed = Event::new(
            "pane.closed",
            Some("w1:p1".into()),
            json!({"pane_id": "w1:p1", "workspace_id": "w1"}),
        );
        for _ in 0..WAITING_PER_PLUGIN + 3 {
            host.run_hooks(&closed);
        }
        assert_eq!(host.waiting.lock().unwrap()["t.q"], WAITING_PER_PLUGIN);
        let log = home.state_of("t.q").join("host.log");
        let logged = std::fs::read_to_string(&log).expect("log");
        assert_eq!(logged.matches("too many hooks waiting").count(), 3);

        // The manifest changed while they waited: none of them runs.
        std::fs::write(&manifest_at, format!("{text}# changed\n")).expect("rewrite");
        drop(turns);
        for _ in 0..100 {
            if host.waiting.lock().unwrap()["t.q"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !folder.join("ran").exists(),
            "a hook ran after its consent went"
        );
        let logged = std::fs::read_to_string(&log).expect("log");
        assert!(logged.contains("not run: no longer agreed to"), "{logged}");
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
        assert!(
            closed.scope.context.get("workspace_label").is_none(),
            "a closed tab is told by its ids only, as herdr tells it"
        );
        let space_closed = events
            .iter()
            .find(|e| e.dotted == "workspace.closed")
            .expect("space closed");
        let env: BTreeMap<_, _> = space_closed.scope.env.clone().into_iter().collect();
        assert_eq!(env["HERDR_WORKSPACE_ID"], "w3");
        assert_eq!(space_closed.scope.context["workspace_label"], "docs");
        assert_eq!(
            space_closed.data["workspace"]["label"], "docs",
            "the space it was"
        );

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

        // A pane closed through the host is told by its pane and space, as herdr tells it.
        let mut state = State::default();
        state.update(
            vec![space(1, "a")],
            vec![row(1, 1, json!({})), row(2, 1, json!({}))],
            &[],
        );
        state.closing.insert(2);
        let events = state.update(vec![space(1, "a")], vec![row(1, 1, json!({}))], &[]);
        let closed = events
            .iter()
            .find(|e| e.dotted == "pane.closed")
            .expect("closed");
        let env: BTreeMap<_, _> = closed.scope.env.clone().into_iter().collect();
        assert_eq!(env["HERDR_PANE_ID"], "w1:p2");
        assert_eq!(env["HERDR_WORKSPACE_ID"], "w1");
        assert!(!env.contains_key("HERDR_TAB_ID"));
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
        assert_eq!(
            lost["error"]["message"],
            "event subscription fell behind retained history; resubscribe and resync with session.snapshot"
        );
        assert!(
            lines.next_line().await.expect("read").is_none(),
            "the stream ends after the loss, as herdr's does"
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
        // Off its own start time, not off `started`: the two spawns can straddle a clock tick
        // (Linux counts in 10 ms ticks, macOS in seconds), and `started + 1` is then exactly
        // when `other` started.
        let other_started =
            qoop_pty::sys::start_time_of(qoop_pty::Pid(other_group)).expect("start time");
        edit_pgids(&home, |groups| {
            groups.push(("t.a".into(), group, started));
            // A recorded group whose pid now belongs to some other process.
            groups.push(("t.b".into(), other_group, other_started.wrapping_add(1)));
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

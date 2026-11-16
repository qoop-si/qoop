//! What a herdr plugin is told about qoop, in herdr's names and shapes.
//!
//! A herdr plugin asks over herdr's socket (or its CLI, which asks the same socket) and reads
//! the answer as herdr 0.9.5 writes it. Everything here is a pure function of what the daemon
//! already lists: the rows, the stored split trees, the spaces. The host (which keeps the tab
//! keys and the focused terminal) feeds them in; nothing here asks the daemon or keeps state.
//!
//! Ids: a space is `w<N>`, a terminal `w<N>:p<T>`, a tab `w<N>:t<K>`, where K is the key the
//! host gave the tab: its first leaf when the host first saw it. A pane is found by T alone,
//! so a plugin holding an id from before a move still reaches the terminal.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use qoop_runtime::control::{AgentRow, Pane, SplitDir, TerminalStream, WorkspaceRow};
use serde_json::{Value, json};

/// The herdr release whose shapes these are, as `ping` and `herdr --version` report it.
pub(crate) const VERSION: &str = "0.9.5";
/// herdr 0.9.5's socket protocol number, as `ping` and `session.snapshot` report it.
pub(crate) const PROTOCOL: u32 = 22;

/// A herdr id, as a plugin names a qoop thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Id {
    /// `w<N>`: space N.
    Workspace(i64),
    /// `w<N>:t<K>`: the tab keyed K in space N.
    Tab { workspace: i64, key: TerminalStream },
    /// `w<N>:p<T>`: terminal T, which space N held when the plugin was told.
    Pane {
        workspace: i64,
        terminal: TerminalStream,
    },
}

impl Id {
    /// The id a plugin sent, or `None` for anything that is not one of the three forms.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (space, rest) = match text.split_once(':') {
            Some((space, rest)) => (space, Some(rest)),
            None => (text, None),
        };
        let workspace = number(space.strip_prefix('w')?)?;
        let workspace = i64::from(workspace);
        let Some(rest) = rest else {
            return Some(Self::Workspace(workspace));
        };
        if let Some(key) = rest.strip_prefix('t') {
            return Some(Self::Tab {
                workspace,
                key: number(key)?,
            });
        }
        Some(Self::Pane {
            workspace,
            terminal: number(rest.strip_prefix('p')?)?,
        })
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(workspace) => write!(f, "w{workspace}"),
            Self::Tab { workspace, key } => write!(f, "w{workspace}:t{key}"),
            Self::Pane {
                workspace,
                terminal,
            } => write!(f, "w{workspace}:p{terminal}"),
        }
    }
}

/// Digits only, above zero, as qoop writes them: no sign, no space, no leading zero, so one
/// thing has one id and `w01` is not a second spelling of `w1`.
fn number(text: &str) -> Option<u32> {
    if text.starts_with('0') || text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn workspace_id(workspace: i64) -> String {
    Id::Workspace(workspace).to_string()
}

fn pane_id(row: &AgentRow) -> String {
    Id::Pane {
        workspace: row.workspace,
        terminal: row.terminal,
    }
    .to_string()
}

/// herdr's agent status for a row. herdr's five words cover qoop's states one to one where
/// they mean the same; `starting`, `failed`, `uncertain` and a terminal with no agent in it
/// are `unknown`, which herdr's own docs read as "do not conclude anything".
pub(crate) fn agent_status(row: &AgentRow) -> &'static str {
    if !row.agent {
        return "unknown";
    }
    match row.state.as_str() {
        "working" => "working",
        "blocked" => "blocked",
        "idle" => "idle",
        "done" => "done",
        _ => "unknown",
    }
}

/// A tab's or a space's status: its most pressing pane's.
fn summary<'a>(rows: impl Iterator<Item = &'a AgentRow>) -> &'static str {
    const ORDER: [&str; 5] = ["blocked", "working", "done", "idle", "unknown"];
    rows.map(agent_status)
        .min_by_key(|status| ORDER.iter().position(|o| o == status))
        .unwrap_or("unknown")
}

/// The events a manifest's `[[events]] on` names, dotted, and the name a handler is told in
/// `HERDR_PLUGIN_EVENT` and `HERDR_PLUGIN_EVENT_JSON`: herdr's `EventKind`, all underscored.
pub(crate) const HOOK_EVENTS: &[(&str, &str)] = &[
    ("workspace.created", "workspace_created"),
    ("workspace.updated", "workspace_updated"),
    ("workspace.metadata_updated", "workspace_metadata_updated"),
    ("workspace.closed", "workspace_closed"),
    ("workspace.renamed", "workspace_renamed"),
    ("workspace.moved", "workspace_moved"),
    ("workspace.reordered", "workspace_reordered"),
    ("workspace.focused", "workspace_focused"),
    ("worktree.created", "worktree_created"),
    ("worktree.opened", "worktree_opened"),
    ("worktree.removed", "worktree_removed"),
    ("tab.created", "tab_created"),
    ("tab.closed", "tab_closed"),
    ("tab.renamed", "tab_renamed"),
    ("tab.moved", "tab_moved"),
    ("tab.focused", "tab_focused"),
    ("pane.created", "pane_created"),
    ("pane.closed", "pane_closed"),
    ("pane.updated", "pane_updated"),
    ("pane.focused", "pane_focused"),
    ("pane.moved", "pane_moved"),
    ("pane.output_changed", "pane_output_changed"),
    ("pane.exited", "pane_exited"),
    ("pane.agent_detected", "pane_agent_detected"),
    ("pane.agent_status_changed", "pane_agent_status_changed"),
    ("layout.updated", "layout_updated"),
];

/// The types `events.subscribe` takes, dotted, and the `event` name the stream writes for
/// each. Underscored like the hooks', except the three per-pane subscriptions, which herdr
/// writes as its `SubscriptionEventKind`, dotted.
pub(crate) const SUBSCRIPTIONS: &[(&str, &str)] = &[
    ("workspace.created", "workspace_created"),
    ("workspace.updated", "workspace_updated"),
    ("workspace.metadata_updated", "workspace_metadata_updated"),
    ("workspace.renamed", "workspace_renamed"),
    ("workspace.moved", "workspace_moved"),
    ("workspace.reordered", "workspace_reordered"),
    ("workspace.closed", "workspace_closed"),
    ("workspace.focused", "workspace_focused"),
    ("worktree.created", "worktree_created"),
    ("worktree.opened", "worktree_opened"),
    ("worktree.removed", "worktree_removed"),
    ("tab.created", "tab_created"),
    ("tab.closed", "tab_closed"),
    ("tab.focused", "tab_focused"),
    ("tab.renamed", "tab_renamed"),
    ("tab.moved", "tab_moved"),
    ("pane.created", "pane_created"),
    ("pane.closed", "pane_closed"),
    ("pane.updated", "pane_updated"),
    ("pane.focused", "pane_focused"),
    ("pane.moved", "pane_moved"),
    ("pane.exited", "pane_exited"),
    ("pane.agent_detected", "pane_agent_detected"),
    ("pane.output_matched", "pane.output_matched"),
    ("pane.agent_status_changed", "pane.agent_status_changed"),
    ("pane.scroll_changed", "pane.scroll_changed"),
    ("layout.updated", "layout_updated"),
];

/// The written name for a dotted one in `table`, `None` for one herdr does not have there.
pub(crate) fn written(table: &[(&str, &'static str)], dotted: &str) -> Option<&'static str> {
    table.iter().find(|(d, _)| *d == dotted).map(|(_, w)| *w)
}

/// The dotted name for a written one in `table`, `None` for one herdr does not have there.
pub(crate) fn dotted(table: &[(&'static str, &str)], written: &str) -> Option<&'static str> {
    table.iter().find(|(_, w)| *w == written).map(|(d, _)| *d)
}

/// The socket methods qoop answers, in herdr's names: what a pinned plugin calls and Phase 1
/// serves (`evals/herdr-compat/calls.md`). Every other method is refused as herdr refuses one
/// it does not have.
pub(crate) const METHODS: &[&str] = &[
    "ping",
    "session.snapshot",
    "pane.list",
    "pane.get",
    "pane.current",
    "pane.read",
    "pane.layout",
    "pane.split",
    "pane.close",
    "pane.focus",
    "pane.send_text",
    "pane.send_input",
    "pane.process_info",
    "pane.rename",
    "pane.zoom",
    "pane.swap",
    "pane.focus_direction",
    "pane.resize",
    "pane.report_metadata",
    "tab.list",
    "tab.get",
    "tab.create",
    "tab.rename",
    "tab.focus",
    "tab.close",
    "workspace.list",
    "workspace.get",
    "workspace.create",
    "workspace.focus",
    "workspace.rename",
    "workspace.close",
    "workspace.report_metadata",
    "agent.list",
    "agent.get",
    "agent.focus",
    "agent.prompt",
    "agent.rename",
    "notification.show",
    "plugin.list",
    "plugin.action.list",
    "plugin.action.invoke",
    "plugin.pane.open",
    "plugin.pane.close",
    "popup.close",
    "server.reload_config",
    "events.subscribe",
];

/// A success line: `result` carries its own `type`.
pub(crate) fn reply(id: &str, result: Value) -> Value {
    json!({"id": id, "result": result})
}

/// A refusal line, in herdr's `{code, message}`.
pub(crate) fn refuse(id: &str, code: &str, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

/// What herdr answers a method it does not have (its request fails to parse), and what qoop
/// answers every method outside [`METHODS`]. Plugins match on `invalid_request` and on
/// "unknown variant" to fall back; the list after it names what qoop has, not what herdr has.
pub(crate) fn unknown_method(id: &str, method: &str) -> Value {
    let expected: Vec<String> = METHODS.iter().map(|m| format!("`{m}`")).collect();
    let message = format!(
        "invalid request: unknown variant `{method}`, expected one of {}",
        expected.join(", ")
    );
    refuse(id, "invalid_request", &message)
}

/// One line of an event stream, or the JSON a handler is told. An underscored event is herdr's
/// `EventData`, which carries its own name as `type`; the dotted per-pane subscription events
/// carry none.
pub(crate) fn event(name: &str, mut data: Value) -> Value {
    if !name.contains('.')
        && let Value::Object(fields) = &mut data
    {
        fields.insert("type".into(), name.into());
    }
    json!({"event": name, "data": data})
}

/// One tab: the leaves of a stored split tree, or a terminal alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    pub(crate) workspace: i64,
    /// The `K` of `w<N>:t<K>`. [`tabs`] keys a tab by its first leaf; the host replaces it
    /// with the key it minted when the tab's first leaf has since gone.
    pub(crate) key: TerminalStream,
    pub(crate) leaves: Vec<TerminalStream>,
    pub(crate) tree: Option<Pane>,
}

impl Tab {
    pub(crate) fn id(&self) -> String {
        Id::Tab {
            workspace: self.workspace,
            key: self.key,
        }
        .to_string()
    }
}

/// The tabs the rows make, in row order. A terminal in a stored split tree shares its tab with
/// the tree's other leaves (the daemon sends trees cut to the living terminals); any other
/// terminal is a tab of its own. A leaf that two stored trees both hold stays in the tab that
/// took it first, so no terminal is in two tabs.
pub(crate) fn tabs(rows: &[AgentRow], splits: &[Pane]) -> Vec<Tab> {
    let mut out: Vec<Tab> = Vec::new();
    for row in rows {
        let placed = |t: TerminalStream| out.iter().any(|tab| tab.leaves.contains(&t));
        if placed(row.terminal) {
            continue;
        }
        let tree = splits
            .iter()
            .find(|tree| tree.leaves().contains(&row.terminal))
            .and_then(|tree| tree.retain(&|t| !placed(t)))
            .filter(|tree| matches!(tree, Pane::Split { .. }));
        let leaves = tree
            .as_ref()
            .map_or_else(|| vec![row.terminal], Pane::leaves);
        out.push(Tab {
            workspace: row.workspace,
            key: leaves[0],
            leaves,
            tree,
        });
    }
    out
}

/// A tab's own name: the qoop name of its first terminal, the one `tab.rename` sets.
pub(crate) fn tab_name(tab: &Tab, rows: &[AgentRow]) -> Option<String> {
    rows.iter()
        .find(|row| row.terminal == tab.leaves[0])
        .and_then(|row| row.name.clone())
}

/// A tab's label as herdr gives it: its name, else its number in its space.
pub(crate) fn tab_label(tab: &Tab, rows: &[AgentRow], number: usize) -> String {
    tab_name(tab, rows).unwrap_or_else(|| number.to_string())
}

/// Pane labels and agent names a plugin gave, by terminal: kept by the host apart from the
/// qoop name, which is the tab's, as herdr keeps the three apart.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Names {
    #[serde(default)]
    pub(crate) panes: BTreeMap<TerminalStream, String>,
    #[serde(default)]
    pub(crate) agents: BTreeMap<TerminalStream, String>,
}

/// The agent a row runs, only when the daemon calls it one: its program's name, else what
/// stands in front of it, else plain `agent`, so an agent never reads as no agent.
pub(crate) fn agent_name(row: &AgentRow) -> Option<&str> {
    row.agent.then(|| {
        row.program
            .as_deref()
            .or(row.behind.as_deref())
            .unwrap_or("agent")
    })
}

/// herdr's `agent_session`: the conversation id the agent reported, when it did.
fn agent_session(row: &AgentRow) -> Value {
    match (agent_name(row), row.agent_session.as_deref()) {
        (Some(agent), Some(value)) => {
            json!({"source": "qoop", "agent": agent, "kind": "id", "value": value})
        }
        _ => Value::Null,
    }
}

/// A herdr info object as herdr writes it: a field with nothing in it (`None`, an empty map)
/// is left out, not written as `null` or `{}`.
pub(crate) fn compact(mut value: Value) -> Value {
    if let Value::Object(fields) = &mut value {
        fields
            .retain(|_, v| !(v.is_null() || v.as_object().is_some_and(serde_json::Map::is_empty)));
    }
    value
}

/// herdr's `PaneInfo` for a row. `revision` is the caller's: the host keeps a count per pane
/// that rises on every change to its row (state, title, cwd, generation), since qoop counts no
/// output revisions and a plugin such as auto-title reads a revision change as activity. `cwd`
/// is where the row says the terminal is; the host puts the foreground process's own in its
/// place when it reads one. The terminal's title is the agent's conversation title, the only
/// title a row carries.
pub(crate) fn pane_info(row: &AgentRow, tab: &Tab, focused: bool, revision: u64) -> Value {
    let agent = agent_name(row);
    compact(json!({
        "pane_id": pane_id(row),
        "terminal_id": row.terminal.to_string(),
        "workspace_id": workspace_id(row.workspace),
        "tab_id": tab.id(),
        "focused": focused,
        "cwd": row.cwd,
        "foreground_cwd": row.cwd,
        "agent": agent,
        "display_agent": agent,
        "agent_status": agent_status(row),
        "agent_session": agent_session(row),
        "title": row.agent_title,
        "terminal_title": row.agent_title,
        "terminal_title_stripped": row.agent_title,
        "revision": revision,
        "tokens": {},
        "state_labels": {},
    }))
}

/// herdr's `AgentInfo` for a row the daemon calls an agent; `revision` as for [`pane_info`].
pub(crate) fn agent_info(row: &AgentRow, tab: &Tab, focused: bool, revision: u64) -> Value {
    let agent = agent_name(row);
    compact(json!({
        "terminal_id": row.terminal.to_string(),
        "pane_id": pane_id(row),
        "workspace_id": workspace_id(row.workspace),
        "tab_id": tab.id(),
        "focused": focused,
        "agent": agent,
        "display_agent": agent,
        "agent_status": agent_status(row),
        "agent_session": agent_session(row),
        "cwd": row.cwd,
        "foreground_cwd": row.cwd,
        "title": row.agent_title,
        "terminal_title": row.agent_title,
        "terminal_title_stripped": row.agent_title,
        "revision": revision,
        "tokens": {},
        "state_labels": {},
    }))
}

/// herdr's `TabInfo`. `number` counts from one within the space, in tab order.
pub(crate) fn tab_info(tab: &Tab, number: usize, rows: &[AgentRow], focused: bool) -> Value {
    json!({
        "tab_id": tab.id(),
        "workspace_id": workspace_id(tab.workspace),
        "number": number,
        "label": tab_label(tab, rows, number),
        "focused": focused,
        "pane_count": tab.leaves.len(),
        "agent_status": summary(rows.iter().filter(|row| tab.leaves.contains(&row.terminal))),
    })
}

/// herdr's `WorkspaceInfo`. `number` counts from one in the spaces' order. `active` is the
/// focused tab when it is in this space, else the space's first: qoop knows only the one
/// terminal the interface shows, not which tab each space last showed. A space with no tab
/// awake has no tab to name, and says so with an empty id.
pub(crate) fn workspace_info(
    space: &WorkspaceRow,
    number: usize,
    tabs: &[Tab],
    rows: &[AgentRow],
    focused: bool,
    active: Option<&Tab>,
) -> Value {
    let own: Vec<&Tab> = tabs.iter().filter(|t| t.workspace == space.id).collect();
    let active = active
        .filter(|t| t.workspace == space.id)
        .or(own.first().copied())
        .map(Tab::id);
    json!({
        "workspace_id": workspace_id(space.id),
        "number": number,
        "label": space_label(space, rows),
        "focused": focused,
        "pane_count": own.iter().map(|t| t.leaves.len()).sum::<usize>(),
        "tab_count": own.len(),
        "active_tab_id": active.unwrap_or_default(),
        "agent_status": summary(rows.iter().filter(|row| row.workspace == space.id)),
    })
}

/// A space's label: its name, except qoop's default space while still called `default`,
/// which herdr would label by its folder: the space's own folder, else its first pane's.
pub(crate) fn space_label(space: &WorkspaceRow, rows: &[AgentRow]) -> String {
    if space.closable || space.name != "default" {
        return space.name.clone();
    }
    let folder = space.dir.clone().or_else(|| {
        rows.iter()
            .find(|r| r.workspace == space.id)
            .and_then(|r| r.cwd.clone())
    });
    folder
        .as_deref()
        .and_then(|d| std::path::Path::new(d).file_name())
        .map_or_else(|| space.name.clone(), |n| n.to_string_lossy().into_owned())
}

/// A cell rectangle, as herdr's layouts give one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Rect {
    fn json(self) -> Value {
        json!({"x": self.x, "y": self.y, "width": self.width, "height": self.height})
    }

    /// The two sides of a split at `permille` of the way along: side by side for `V`.
    fn cut(self, dir: SplitDir, permille: u16) -> (Self, Self) {
        let part = |whole: u32| (u64::from(whole) * u64::from(permille.min(1000)) / 1000) as u32;
        match dir {
            SplitDir::V => {
                let first = part(self.width);
                (
                    Self {
                        width: first,
                        ..self
                    },
                    Self {
                        x: self.x + first,
                        width: self.width - first,
                        ..self
                    },
                )
            }
            SplitDir::H => {
                let first = part(self.height);
                (
                    Self {
                        height: first,
                        ..self
                    },
                    Self {
                        y: self.y + first,
                        height: self.height - first,
                        ..self
                    },
                )
            }
        }
    }
}

/// herdr's `PaneLayoutSnapshot` for a tab, its panes placed in `area` by the stored ratios.
/// qoop keeps no pane sizes, so the rectangles are the tree's proportions of the area the
/// caller names, not cells on anyone's screen. A split's id is its path from the root, `r`
/// then `0` for a first side and `1` for a second.
pub(crate) fn layout(
    tab: &Tab,
    area: Rect,
    focused: Option<TerminalStream>,
    zoomed: bool,
) -> Value {
    // Each tab has a focused pane of its own, as in herdr: the one shown, else its first.
    let focused_pane = focused
        .filter(|t| tab.leaves.contains(t))
        .unwrap_or(tab.leaves[0]);
    let mut panes = Vec::new();
    let mut splits = Vec::new();
    match &tab.tree {
        Some(tree) => place(
            tab.workspace,
            tree,
            area,
            String::new(),
            Some(focused_pane),
            &mut panes,
            &mut splits,
        ),
        None => panes.push(leaf(tab.workspace, tab.leaves[0], area, Some(focused_pane))),
    }
    json!({
        "workspace_id": workspace_id(tab.workspace),
        "tab_id": tab.id(),
        "zoomed": zoomed,
        "area": area.json(),
        "focused_pane_id": Id::Pane { workspace: tab.workspace, terminal: focused_pane }.to_string(),
        "panes": panes,
        "splits": splits,
    })
}

fn leaf(
    workspace: i64,
    terminal: TerminalStream,
    rect: Rect,
    focused: Option<TerminalStream>,
) -> Value {
    json!({
        "pane_id": Id::Pane { workspace, terminal }.to_string(),
        "focused": focused == Some(terminal),
        "rect": rect.json(),
    })
}

fn place(
    workspace: i64,
    tree: &Pane,
    rect: Rect,
    path: String,
    focused: Option<TerminalStream>,
    panes: &mut Vec<Value>,
    splits: &mut Vec<Value>,
) {
    match tree {
        Pane::Leaf { terminal } => panes.push(leaf(workspace, *terminal, rect, focused)),
        Pane::Split {
            dir,
            permille,
            first,
            second,
            ..
        } => {
            let (a, b) = rect.cut(*dir, *permille);
            let direction = match dir {
                SplitDir::V => "right",
                SplitDir::H => "down",
            };
            // herdr's id: the split's place in this walk, then its path from the root (0 for a
            // first child, 1 for a second), `root` for the root.
            let id = if path.is_empty() {
                format!("split_{}_root", splits.len())
            } else {
                format!("split_{}_{path}", splits.len())
            };
            splits.push(json!({
                "id": id,
                "direction": direction,
                "ratio": f64::from(*permille) / 1000.0,
                "rect": rect.json(),
            }));
            place(
                workspace,
                first,
                a,
                format!("{path}0"),
                focused,
                panes,
                splits,
            );
            place(
                workspace,
                second,
                b,
                format!("{path}1"),
                focused,
                panes,
                splits,
            );
        }
    }
}

/// One process of a terminal's foreground group, as the host reads it from the process table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Process {
    pub(crate) pid: i32,
    pub(crate) name: String,
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: Option<String>,
}

/// herdr's `PaneProcessInfo` for a row and its foreground group's processes.
pub(crate) fn process_info(row: &AgentRow, foreground: &[Process]) -> Value {
    let processes: Vec<Value> = foreground
        .iter()
        .map(|p| {
            // As herdr gives them: the program's name, not its path (a login shell's `-zsh`
            // is `zsh`), and the line as one string.
            let argv0 = p.argv.first().map(|a| {
                let a = a.strip_prefix('-').unwrap_or(a);
                std::path::Path::new(a)
                    .file_name()
                    .map_or_else(|| a.to_string(), |n| n.to_string_lossy().into_owned())
            });
            json!({
                "pid": p.pid,
                "name": p.name,
                "argv0": argv0,
                "argv": p.argv,
                "cmdline": (!p.argv.is_empty()).then(|| p.argv.join(" ")),
                "cwd": p.cwd,
            })
        })
        .collect();
    json!({
        "pane_id": pane_id(row),
        "shell_pid": row.shell_pid,
        "foreground_process_group_id": row.pgid,
        "foreground_processes": processes,
    })
}

/// What the daemon lists, plus what only the host knows: the terminal the interface shows and
/// each pane's revision (a terminal it has not counted is at zero).
pub(crate) struct View<'a> {
    pub(crate) spaces: &'a [WorkspaceRow],
    pub(crate) rows: &'a [AgentRow],
    pub(crate) tabs: &'a [Tab],
    pub(crate) focused: Option<TerminalStream>,
    pub(crate) revisions: &'a BTreeMap<TerminalStream, u64>,
    pub(crate) tokens: Tokens,
    /// Where the interface lays tabs out, as it said (`qoop.focus`), else a nominal area.
    pub(crate) area: Rect,
    /// The panes zoomed in their tabs.
    pub(crate) zoomed: BTreeSet<TerminalStream>,
    /// How many times each pane's agent status changed, as herdr's `state_change_seq`.
    pub(crate) seqs: BTreeMap<TerminalStream, u64>,
    /// Pane labels and agent names plugins gave.
    pub(crate) names: Names,
    /// How far each pane can scroll back and is scrolled back.
    pub(crate) scrolls: Scrolls,
}

/// herdr's `scroll` for each pane: the rows kept behind its screen, and the one pane the
/// interface shows scrolled back (qoop scrolls only the pane in front of a person).
#[derive(Clone, Debug, Default)]
pub(crate) struct Scrolls {
    pub(crate) history: BTreeMap<TerminalStream, u64>,
    pub(crate) back: Option<(TerminalStream, u64)>,
}

/// Metadata tokens alive now, by terminal and by space, as `tokens` in the infos.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tokens {
    pub(crate) panes: BTreeMap<TerminalStream, BTreeMap<String, String>>,
    pub(crate) spaces: BTreeMap<i64, BTreeMap<String, String>>,
}

impl View<'_> {
    fn tab_of(&self, terminal: TerminalStream) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.leaves.contains(&terminal))
    }

    fn revision(&self, terminal: TerminalStream) -> u64 {
        self.revisions.get(&terminal).copied().unwrap_or(0)
    }

    /// Whether a pane of `tab` is zoomed.
    pub(crate) fn zoomed_tab(&self, tab: &Tab) -> bool {
        tab.leaves.iter().any(|t| self.zoomed.contains(t))
    }

    fn focused_tab(&self) -> Option<&Tab> {
        self.focused.and_then(|t| self.tab_of(t))
    }

    /// Every pane, in row order, for `pane.list` and the snapshot.
    pub(crate) fn panes(&self) -> Vec<Value> {
        self.rows
            .iter()
            .filter_map(|row| {
                let tab = self.tab_of(row.terminal)?;
                let mut info = pane_info(
                    row,
                    tab,
                    self.focused == Some(row.terminal),
                    self.revision(row.terminal),
                );
                super::tokens::put(&mut info, self.tokens.panes.get(&row.terminal));
                info["scroll"] = self.scroll(tab, row.terminal);
                if let Some(label) = self.names.panes.get(&row.terminal) {
                    info["label"] = json!(label);
                }
                Some(info)
            })
            .collect()
    }

    /// herdr's `scroll` for a pane: qoop's terminals stay at their bottom (the interface reads
    /// what went past from the history on disk, never by scrolling the terminal), so none is
    /// scrolled and none can be; the rows are the pane's in its tab's layout.
    fn scroll(&self, tab: &Tab, terminal: TerminalStream) -> Value {
        let laid = layout(tab, self.area, self.focused, self.zoomed_tab(tab));
        let rows = laid["panes"]
            .as_array()
            .and_then(|panes| {
                let id = Id::Pane {
                    workspace: tab.workspace,
                    terminal,
                }
                .to_string();
                panes.iter().find(|p| p["pane_id"] == id.as_str())
            })
            .and_then(|p| p["rect"]["height"].as_u64())
            .unwrap_or(0);
        let most = self.scrolls.history.get(&terminal).copied().unwrap_or(0);
        let back = (self.scrolls.back)
            .filter(|(t, _)| *t == terminal)
            .map_or(0, |(_, rows)| rows.min(most));
        json!({"offset_from_bottom": back, "max_offset_from_bottom": most, "viewport_rows": rows})
    }

    /// Every tab, numbered within its space.
    pub(crate) fn tab_infos(&self) -> Vec<Value> {
        let focused = self.focused_tab().map(Tab::id);
        let mut out = Vec::new();
        for space in self.spaces {
            let own = self.tabs.iter().filter(|t| t.workspace == space.id);
            for (i, tab) in own.enumerate() {
                let id = tab.id();
                out.push(tab_info(
                    tab,
                    i + 1,
                    self.rows,
                    focused.as_ref() == Some(&id),
                ));
            }
        }
        out
    }

    /// Every space, numbered in the daemon's order.
    pub(crate) fn workspace_infos(&self) -> Vec<Value> {
        let focused = self.focused_tab();
        self.spaces
            .iter()
            .enumerate()
            .map(|(i, space)| {
                let here = focused.filter(|tab| tab.workspace == space.id);
                let mut info =
                    workspace_info(space, i + 1, self.tabs, self.rows, here.is_some(), here);
                super::tokens::put(&mut info, self.tokens.spaces.get(&space.id));
                info
            })
            .collect()
    }

    /// Every agent, for `agent.list` and the snapshot.
    pub(crate) fn agents(&self) -> Vec<Value> {
        self.rows
            .iter()
            .filter(|row| row.agent)
            .filter_map(|row| {
                let tab = self.tab_of(row.terminal)?;
                let mut info = agent_info(
                    row,
                    tab,
                    self.focused == Some(row.terminal),
                    self.revision(row.terminal),
                );
                super::tokens::put(&mut info, self.tokens.panes.get(&row.terminal));
                info["state_change_seq"] =
                    json!(self.seqs.get(&row.terminal).copied().unwrap_or(0));
                if let Some(name) = self.names.agents.get(&row.terminal) {
                    info["name"] = json!(name);
                }
                Some(info)
            })
            .collect()
    }

    /// herdr's `SessionSnapshot`, with every tab laid out in `area`.
    pub(crate) fn snapshot(&self, area: Rect) -> Value {
        let focused_tab = self.focused_tab();
        let layouts: Vec<Value> = self
            .tabs
            .iter()
            .map(|tab| layout(tab, area, self.focused, self.zoomed_tab(tab)))
            .collect();
        let focused_pane = self.focused.zip(focused_tab).map(|(terminal, tab)| {
            Id::Pane {
                workspace: tab.workspace,
                terminal,
            }
            .to_string()
        });
        json!({
            "version": VERSION,
            "protocol": PROTOCOL,
            "workspaces": self.workspace_infos(),
            "tabs": self.tab_infos(),
            "panes": self.panes(),
            "layouts": layouts,
            "agents": self.agents(),
            "focused_workspace_id": focused_tab.map(|tab| workspace_id(tab.workspace)),
            "focused_tab_id": focused_tab.map(Tab::id),
            "focused_pane_id": focused_pane,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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
            dir: SplitDir::V,
            permille: 500,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: first }),
            second: Box::new(Pane::Leaf { terminal: second }),
        }
    }

    #[test]
    fn the_default_space_is_labelled_by_its_folder() {
        let rows = [row(1, 1, json!({"cwd": "/src/repo"}))];
        // qoop's default space: not closable, still called `default`.
        let default = space(1, "default");
        assert_eq!(
            space_label(&default, &rows),
            "repo",
            "its first pane's folder"
        );
        let mut with_dir = space(1, "default");
        with_dir.dir = Some("/work/api".into());
        assert_eq!(space_label(&with_dir, &rows), "api", "its own folder first");
        let named: WorkspaceRow = serde_json::from_value(
            json!({"id": 2, "name": "default", "terminals": 0, "closable": true}),
        )
        .expect("a space");
        assert_eq!(
            space_label(&named, &rows),
            "default",
            "a space a person called default"
        );
        assert_eq!(space_label(&space(1, "api"), &rows), "api");
        assert_eq!(
            space_label(&space(1, "default"), &[]),
            "default",
            "no folder known"
        );
    }

    #[test]
    fn ids_round_trip_and_refuse_what_qoop_never_hands_out() {
        for text in ["w1", "w12:p7", "w3:t40"] {
            assert_eq!(Id::parse(text).expect(text).to_string(), text);
        }
        assert_eq!(
            Id::parse("w2:p9"),
            Some(Id::Pane {
                workspace: 2,
                terminal: 9
            })
        );
        assert_eq!(
            Id::parse("w2:t9"),
            Some(Id::Tab {
                workspace: 2,
                key: 9
            })
        );
        for bad in [
            "",
            "w",
            "w0",
            "w-1",
            "w+1",
            "1",
            "p1",
            "w1:",
            "w1:p",
            "w1:p0",
            "w1:x3",
            "w1:p3:p4",
            "w1:p 3",
            "W1",
            "w99999999999",
            "w01",
            "w1:p007",
            "w1:t00",
        ] {
            assert_eq!(Id::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn agent_status_maps_the_four_shared_words_and_nothing_else() {
        let agent = |state: &str| row(1, 1, json!({"agent": true, "state": state}));
        for same in ["working", "blocked", "idle", "done"] {
            assert_eq!(agent_status(&agent(same)), same);
        }
        for other in ["starting", "failed", "uncertain", "unknown", "sleeping"] {
            assert_eq!(agent_status(&agent(other)), "unknown", "{other}");
        }
        assert_eq!(
            agent_status(&row(1, 1, json!({"state": "working"}))),
            "unknown"
        );
    }

    #[test]
    fn event_names_map_both_ways() {
        // Handlers hear herdr's EventKind: all underscored, pane_output_changed among them,
        // and none of the per-pane subscription events.
        let hook = |on| written(HOOK_EVENTS, on);
        assert_eq!(hook("pane.exited"), Some("pane_exited"));
        assert_eq!(hook("pane.closed"), Some("pane_closed"));
        assert_eq!(
            hook("pane.agent_status_changed"),
            Some("pane_agent_status_changed")
        );
        assert_eq!(hook("pane.output_changed"), Some("pane_output_changed"));
        assert_eq!(hook("pane.output_matched"), None);
        assert_eq!(hook("pane.scroll_changed"), None);
        assert_eq!(dotted(HOOK_EVENTS, "tab_closed"), Some("tab.closed"));
        assert_eq!(HOOK_EVENTS.len(), 26);
        assert!(HOOK_EVENTS.iter().all(|(_, w)| !w.contains('.')));

        // A subscription stream keeps the dot for exactly the three per-pane events.
        let sub = |on| written(SUBSCRIPTIONS, on);
        for per_pane in [
            "pane.output_matched",
            "pane.agent_status_changed",
            "pane.scroll_changed",
        ] {
            assert_eq!(sub(per_pane), Some(per_pane));
        }
        assert_eq!(sub("pane.exited"), Some("pane_exited"));
        assert_eq!(sub("pane.closed"), Some("pane_closed"));
        assert_eq!(sub("pane.output_changed"), None);
        assert_eq!(
            dotted(SUBSCRIPTIONS, "workspace_created"),
            Some("workspace.created")
        );
        assert_eq!(SUBSCRIPTIONS.len(), 27);
        assert_eq!(
            SUBSCRIPTIONS
                .iter()
                .filter(|(_, w)| w.contains('.'))
                .count(),
            3
        );

        for table in [HOOK_EVENTS, SUBSCRIPTIONS] {
            let ons: BTreeSet<&str> = table.iter().map(|(d, _)| *d).collect();
            let names: BTreeSet<&str> = table.iter().map(|(_, w)| *w).collect();
            assert_eq!(ons.len(), table.len(), "a dotted name twice");
            assert_eq!(names.len(), table.len(), "a written name twice");
            for (on, name) in table {
                assert_eq!(written(table, on), Some(*name));
                assert_eq!(dotted(table, name), Some(*on));
            }
        }
    }

    #[test]
    fn envelopes_are_herdrs() {
        assert_eq!(
            reply("a", json!({"type": "pong"})),
            json!({"id": "a", "result": {"type": "pong"}})
        );
        let refused = unknown_method("b", "pane.report_agent");
        assert_eq!(refused["id"], "b");
        assert_eq!(refused["error"]["code"], "invalid_request");
        let message = refused["error"]["message"].as_str().expect("a message");
        assert!(
            message.starts_with(
                "invalid request: unknown variant `pane.report_agent`, expected one of `ping`, "
            ),
            "{message}"
        );
        assert!(message.ends_with("`events.subscribe`"), "{message}");
        assert_eq!(
            event(
                "tab_closed",
                json!({"tab_id": "w1:t2", "workspace_id": "w1"})
            ),
            json!({"event": "tab_closed", "data": {"tab_id": "w1:t2", "workspace_id": "w1", "type": "tab_closed"}})
        );
        assert_eq!(
            event(
                "pane.agent_status_changed",
                json!({"pane_id": "w1:p2", "workspace_id": "w1", "agent_status": "done"})
            ),
            json!({"event": "pane.agent_status_changed",
                   "data": {"pane_id": "w1:p2", "workspace_id": "w1", "agent_status": "done"}})
        );
    }

    #[test]
    fn a_split_tree_is_one_tab_and_a_lone_terminal_another() {
        let rows = [
            row(3, 1, json!({})),
            row(4, 1, json!({})),
            row(5, 2, json!({})),
        ];
        let splits = [split(4, 3)];
        let tabs = tabs(&rows, &splits);
        assert_eq!(tabs.len(), 2);
        assert_eq!((tabs[0].key, tabs[0].leaves.clone()), (4, vec![4, 3]));
        assert_eq!(tabs[0].id(), "w1:t4");
        assert_eq!(
            (tabs[1].key, tabs[1].leaves.clone(), tabs[1].tree.clone()),
            (5, vec![5], None)
        );
    }

    #[test]
    fn a_leaf_two_stored_trees_hold_is_in_one_tab() {
        let rows = [
            row(3, 1, json!({})),
            row(4, 1, json!({})),
            row(5, 1, json!({})),
        ];
        let splits = [split(3, 4), split(4, 5)];
        let tabs = tabs(&rows, &splits);
        let leaves: Vec<Vec<u32>> = tabs.iter().map(|t| t.leaves.clone()).collect();
        assert_eq!(leaves, [vec![3, 4], vec![5]]);
        assert_eq!(tabs[1].tree, None, "a tree cut to one leaf is no split");
    }

    #[test]
    fn a_lone_survivor_is_laid_out_under_its_own_id() {
        // The tab's first leaf closed: the host keeps the tab's key, the survivor is 9.
        let tab = Tab {
            workspace: 1,
            key: 4,
            leaves: vec![9],
            tree: None,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 5,
        };
        let laid = layout(&tab, area, None, false);
        assert_eq!(laid["tab_id"], "w1:t4");
        assert_eq!(laid["focused_pane_id"], "w1:p9");
        assert_eq!(laid["panes"][0]["pane_id"], "w1:p9");
    }

    #[test]
    fn an_agent_with_no_program_still_reads_as_an_agent() {
        let tab = Tab {
            workspace: 1,
            key: 2,
            leaves: vec![2],
            tree: None,
        };
        let behind = row(2, 1, json!({"agent": true, "behind": "docker"}));
        assert_eq!(pane_info(&behind, &tab, false, 0)["agent"], "docker");
        let bare = row(2, 1, json!({"agent": true}));
        assert_eq!(pane_info(&bare, &tab, false, 0)["agent"], "agent");
    }

    #[test]
    fn a_pane_and_its_agent_read_as_herdr_writes_them() {
        let claude = row(
            7,
            2,
            json!({
                "agent": true, "state": "working", "program": "claude", "name": "api",
                "agent_session": "s-1", "agent_title": "Fix login", "generation": 3,
                "cwd": "/src/api", "pgid": 900, "shell_pid": 880,
            }),
        );
        let tab = Tab {
            workspace: 2,
            key: 7,
            leaves: vec![7],
            tree: None,
        };
        assert_eq!(
            pane_info(&claude, &tab, true, 12),
            json!({
                "pane_id": "w2:p7", "terminal_id": "7", "workspace_id": "w2", "tab_id": "w2:t7",
                "focused": true, "cwd": "/src/api", "foreground_cwd": "/src/api",
                "agent": "claude", "display_agent": "claude", "agent_status": "working",
                "agent_session": {"source": "qoop", "agent": "claude", "kind": "id", "value": "s-1"},
                "title": "Fix login", "terminal_title": "Fix login",
                "terminal_title_stripped": "Fix login", "revision": 12,
            })
        );
        assert_eq!(
            agent_info(&claude, &tab, false, 12),
            json!({
                "terminal_id": "7", "pane_id": "w2:p7", "workspace_id": "w2", "tab_id": "w2:t7",
                "focused": false, "agent": "claude", "display_agent": "claude",
                "agent_status": "working",
                "agent_session": {"source": "qoop", "agent": "claude", "kind": "id", "value": "s-1"},
                "cwd": "/src/api", "foreground_cwd": "/src/api", "title": "Fix login",
                "terminal_title": "Fix login", "terminal_title_stripped": "Fix login",
                "revision": 12,
            })
        );
        let shell = row(8, 2, json!({"program": "zsh", "agent_session": "ignored"}));
        let info = pane_info(&shell, &tab, false, 0);
        assert_eq!(info["agent"], Value::Null);
        assert_eq!(info["agent_session"], Value::Null);
        assert_eq!(info["agent_status"], "unknown");
        assert_eq!(info["revision"], 0);
    }

    #[test]
    fn process_info_names_the_shell_and_the_foreground_group() {
        let r = row(7, 2, json!({"pgid": 900, "shell_pid": 880}));
        let fg = [Process {
            pid: 900,
            name: "node".into(),
            argv: vec!["/usr/local/bin/node".into(), "server.js".into()],
            cwd: Some("/src".into()),
        }];
        assert_eq!(
            process_info(&r, &fg),
            json!({
                "pane_id": "w2:p7", "shell_pid": 880, "foreground_process_group_id": 900,
                "foreground_processes": [
                    {"pid": 900, "name": "node", "argv0": "node",
                     "argv": ["/usr/local/bin/node", "server.js"],
                     "cmdline": "/usr/local/bin/node server.js", "cwd": "/src"},
                ],
            })
        );
        let login = [Process {
            pid: 880,
            name: "zsh".into(),
            argv: vec!["-zsh".into()],
            cwd: None,
        }];
        assert_eq!(
            process_info(&r, &login)["foreground_processes"][0]["argv0"],
            "zsh",
            "a login shell's dash is not its name"
        );
        let gone = process_info(&row(7, 2, json!({})), &[]);
        assert_eq!(gone["shell_pid"], Value::Null);
        assert_eq!(gone["foreground_processes"], json!([]));
    }

    #[test]
    fn a_split_is_laid_out_by_its_ratio() {
        let rows = [row(3, 1, json!({})), row(4, 1, json!({}))];
        let splits = [Pane::Split {
            dir: SplitDir::H,
            permille: 250,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: 3 }),
            second: Box::new(split(4, 9)),
        }];
        let tabs = tabs(&rows, &splits);
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        assert_eq!(
            layout(&tabs[0], area, Some(4), true),
            json!({
                "workspace_id": "w1", "tab_id": "w1:t3", "zoomed": true,
                "area": {"x": 0, "y": 0, "width": 100, "height": 40},
                "focused_pane_id": "w1:p4",
                "panes": [
                    {"pane_id": "w1:p3", "focused": false, "rect": {"x": 0, "y": 0, "width": 100, "height": 10}},
                    {"pane_id": "w1:p4", "focused": true, "rect": {"x": 0, "y": 10, "width": 50, "height": 30}},
                    {"pane_id": "w1:p9", "focused": false, "rect": {"x": 50, "y": 10, "width": 50, "height": 30}},
                ],
                "splits": [
                    {"id": "split_0_root", "direction": "down", "ratio": 0.25, "rect": {"x": 0, "y": 0, "width": 100, "height": 40}},
                    {"id": "split_1_1", "direction": "right", "ratio": 0.5, "rect": {"x": 0, "y": 10, "width": 100, "height": 30}},
                ],
            })
        );
    }

    #[test]
    fn the_snapshot_puts_it_all_together() {
        let rows = [
            row(
                3,
                1,
                json!({"agent": true, "state": "blocked", "program": "codex"}),
            ),
            row(4, 1, json!({"program": "zsh"})),
            row(5, 1, json!({"name": "logs", "program": "tail"})),
        ];
        let splits = [split(3, 4)];
        let tabs = tabs(&rows, &splits);
        let spaces = [space(1, "api"), space(2, "empty")];
        let view = View {
            spaces: &spaces,
            rows: &rows,
            tabs: &tabs,
            focused: Some(4),
            revisions: &BTreeMap::from([(3, 5)]),
            tokens: Tokens {
                panes: BTreeMap::from([(
                    4,
                    BTreeMap::from([("dock".to_string(), "left".to_string())]),
                )]),
                spaces: BTreeMap::from([(
                    1,
                    BTreeMap::from([("kind".to_string(), "api".to_string())]),
                )]),
            },
            area: Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            zoomed: BTreeSet::from([4]),
            seqs: BTreeMap::from([(3, 2)]),
            names: Names {
                panes: BTreeMap::from([(4, "logs pane".to_string())]),
                agents: BTreeMap::from([(3, "reviewer".to_string())]),
            },
            scrolls: Scrolls {
                history: BTreeMap::from([(4, 900), (5, 40)]),
                back: Some((4, 30)),
            },
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let snapshot = view.snapshot(area);
        assert_eq!(snapshot["version"], "0.9.5");
        assert_eq!(snapshot["protocol"], 22);
        assert_eq!(snapshot["focused_workspace_id"], "w1");
        assert_eq!(snapshot["focused_tab_id"], "w1:t3");
        assert_eq!(snapshot["focused_pane_id"], "w1:p4");
        assert_eq!(
            snapshot["workspaces"],
            json!([
                {"workspace_id": "w1", "number": 1, "label": "api", "focused": true,
                 "pane_count": 3, "tab_count": 2, "active_tab_id": "w1:t3",
                 "agent_status": "blocked", "tokens": {"kind": "api"}},
                {"workspace_id": "w2", "number": 2, "label": "empty", "focused": false,
                 "pane_count": 0, "tab_count": 0, "active_tab_id": "",
                 "agent_status": "unknown"},
            ])
        );
        assert_eq!(
            snapshot["tabs"],
            json!([
                {"tab_id": "w1:t3", "workspace_id": "w1", "number": 1, "label": "1",
                 "focused": true, "pane_count": 2, "agent_status": "blocked"},
                {"tab_id": "w1:t5", "workspace_id": "w1", "number": 2, "label": "logs",
                 "focused": false, "pane_count": 1, "agent_status": "unknown"},
            ])
        );
        let panes = snapshot["panes"].as_array().expect("panes");
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[1]["focused"], true);
        assert_eq!(panes[1]["tab_id"], "w1:t3");
        assert_eq!(
            panes[1]["tokens"],
            json!({"dock": "left"}),
            "a pane's tokens"
        );
        assert!(panes[0].get("tokens").is_none(), "none, none shown");
        assert_eq!(panes[1]["label"], "logs pane", "a label a plugin gave");
        assert!(
            panes[2].get("label").is_none(),
            "the qoop name `logs` is its tab's, not the pane's"
        );
        let agents = snapshot["agents"].as_array().expect("agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["pane_id"], "w1:p3");
        assert_eq!(agents[0]["name"], "reviewer", "a name a plugin gave");
        assert_eq!(
            agents[0]["state_change_seq"], 2,
            "herdr's count of status changes"
        );
        assert!(
            agents[0].get("scroll").is_none(),
            "an agent has none, as in herdr"
        );
        assert_eq!(
            panes[2]["scroll"],
            json!({"offset_from_bottom": 0, "max_offset_from_bottom": 40, "viewport_rows": 24}),
            "a pane at its bottom with 40 rows behind it, its rows from the layout"
        );
        assert_eq!(
            panes[1]["scroll"]["offset_from_bottom"], 30,
            "the pane the interface shows scrolled back"
        );
        assert_eq!(panes[1]["scroll"]["max_offset_from_bottom"], 900);
        assert_eq!(
            agents[0]["revision"], 5,
            "the host's count, not the generation"
        );
        assert_eq!(panes[1]["revision"], 0, "an uncounted pane is at zero");
        assert_eq!(snapshot["layouts"].as_array().expect("layouts").len(), 2);
        let laid = &snapshot["layouts"];
        assert_eq!(
            laid[0]["zoomed"], true,
            "the tab with the zoomed pane says so"
        );
        assert_eq!(laid[1]["zoomed"], false);
        assert_eq!(
            laid[0]["splits"][0]["id"], "split_0_root",
            "herdr's split id"
        );
        assert_eq!(
            laid[1]["panes"][0]["focused"], true,
            "a tab's own focused pane"
        );
        assert_eq!(laid[0]["area"]["width"], 80, "the area given");

        let unfocused = View {
            focused: None,
            ..view
        }
        .snapshot(area);
        assert_eq!(unfocused["focused_pane_id"], Value::Null);
        assert_eq!(unfocused["focused_tab_id"], Value::Null);
        assert_eq!(
            unfocused["workspaces"][0]["active_tab_id"], "w1:t3",
            "with nothing focused a space's first tab is its active one"
        );
        let elsewhere = workspace_info(&spaces[1], 2, &tabs, &rows, false, Some(&tabs[0]));
        assert_eq!(
            elsewhere["active_tab_id"], "",
            "another space's tab is never this one's"
        );
    }
}

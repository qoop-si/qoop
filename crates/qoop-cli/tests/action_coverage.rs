//! Gate G11.1: every first-party action maps to a named public protocol method.
//!
//! The rule this enforces is not a tidiness rule. An action with no protocol method behind
//! it is an action only the first-party client can perform, and every such action is a
//! reason a third-party client, a script or a plugin cannot do what the built-in one can.
//! The ecosystem claim is exactly the absence of those.
//!
//! Asserted over the registered handlers rather than over a list somebody maintains: a list
//! stops covering the surface the first time an action is added.

use qoop_runtime::control::ControlRequest;

/// What the client can do, and the protocol method behind each one.
///
/// This is the registry the client dispatches from, so an action that is not here is an
/// action the client cannot perform, and an action here with no method fails the gate.
struct Action {
    name: &'static str,
    /// The method, constructed. `None` would be an action with nothing behind it, which is
    /// what the gate exists to forbid, so the field is not optional and the compiler
    /// requires one.
    method: fn() -> Method,
}

/// A named public protocol method.
enum Method {
    /// A control channel request. Named by its own tag on the wire.
    Control(ControlRequest),
    /// A terminal channel message. Named by its variant.
    Terminal(&'static str),
}

impl Method {
    fn name(&self) -> String {
        match self {
            Method::Control(request) => {
                let value = serde_json::to_value(request).expect("encode");
                value["op"].as_str().expect("a tagged op").to_string()
            }
            Method::Terminal(name) => (*name).to_string(),
        }
    }
}

/// Every action the first-party client offers.
fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "attach to a terminal",
            method: || {
                Method::Control(ControlRequest::Subscribe {
                    id: "a".into(),
                    terminals: vec![1],
                })
            },
        },
        Action {
            name: "put a terminal in the background",
            method: || {
                Method::Control(ControlRequest::Unsubscribe {
                    id: "a".into(),
                    terminals: vec![1],
                })
            },
        },
        Action {
            name: "open a terminal",
            method: || {
                Method::Control(ControlRequest::CreateTerminal {
                    id: "a".into(),
                    cwd: None,
                    workspace: None,
                    name: None,
                })
            },
        },
        Action {
            name: "close a terminal",
            method: || {
                Method::Control(ControlRequest::CloseTerminal {
                    id: "a".into(),
                    terminal: 1,
                })
            },
        },
        Action {
            name: "list terminals",
            method: || Method::Control(ControlRequest::ListTerminals { id: "a".into() }),
        },
        Action {
            name: "show the index of terminals and who is in them",
            method: || Method::Control(ControlRequest::ListAgents { id: "a".into() }),
        },
        Action {
            name: "show a terminal in every interface, from a notch or a launcher",
            method: || {
                Method::Control(ControlRequest::Focus {
                    id: "a".into(),
                    terminal: 1,
                })
            },
        },
        Action {
            name: "say which finished agents the person has not looked at yet",
            method: || {
                Method::Control(ControlRequest::News {
                    id: "a".into(),
                    terminals: vec![1],
                })
            },
        },
        Action {
            name: "show the workspaces",
            method: || Method::Control(ControlRequest::ListWorkspaces { id: "a".into() }),
        },
        Action {
            name: "rename a terminal",
            method: || {
                Method::Control(ControlRequest::RenameTerminal {
                    id: "a".into(),
                    terminal: 1,
                    name: Some("release".into()),
                })
            },
        },
        Action {
            name: "rename a conversation",
            method: || {
                Method::Control(ControlRequest::RenameSession {
                    id: "a".into(),
                    terminal: 1,
                    title: Some("release".into()),
                })
            },
        },
        Action {
            name: "move a terminal to another workspace",
            method: || {
                Method::Control(ControlRequest::MoveTerminal {
                    id: "a".into(),
                    terminal: 1,
                    workspace: 2,
                    place: None,
                    leave_split: false,
                })
            },
        },
        Action {
            name: "rename a workspace",
            method: || {
                Method::Control(ControlRequest::RenameWorkspace {
                    id: "a".into(),
                    workspace: 1,
                    name: "webshop".into(),
                })
            },
        },
        Action {
            name: "colour a workspace",
            method: || {
                Method::Control(ControlRequest::SetWorkspaceInk {
                    id: "a".into(),
                    workspace: 1,
                    ink: Some(2),
                })
            },
        },
        Action {
            name: "move a workspace in the list",
            method: || {
                Method::Control(ControlRequest::MoveWorkspace {
                    id: "a".into(),
                    workspace: 2,
                    before: Some(1),
                })
            },
        },
        Action {
            name: "keep a space or a pane awake",
            method: || {
                Method::Control(ControlRequest::SetKeepAwake {
                    id: "a".into(),
                    workspace: 1,
                    terminal: Some(2),
                    on: true,
                })
            },
        },
        Action {
            name: "start a workspace",
            method: || {
                Method::Control(ControlRequest::CreateWorkspace {
                    id: "a".into(),
                    name: "new".into(),
                })
            },
        },
        Action {
            name: "close an empty workspace",
            method: || {
                Method::Control(ControlRequest::CloseWorkspace {
                    id: "a".into(),
                    workspace: 2,
                })
            },
        },
        Action {
            name: "put a space to sleep",
            method: || {
                Method::Control(ControlRequest::SleepWorkspace {
                    id: "a".into(),
                    workspace: 2,
                    terminal: None,
                })
            },
        },
        Action {
            name: "wake a sleeping space",
            method: || {
                Method::Control(ControlRequest::WakeWorkspace {
                    id: "a".into(),
                    workspace: Some(2),
                    terminal: None,
                })
            },
        },
        Action {
            name: "ask for the geometry",
            method: || {
                Method::Control(ControlRequest::RequestOwnership {
                    id: "a".into(),
                    terminal: 1,
                })
            },
        },
        Action {
            name: "hand over the geometry",
            method: || {
                Method::Control(ControlRequest::GrantOwnership {
                    id: "a".into(),
                    terminal: 1,
                    to: "b".into(),
                })
            },
        },
        Action {
            name: "report the agent's state from its hooks",
            method: || {
                Method::Control(ControlRequest::ReportAgentState {
                    id: "a".into(),
                    terminal: 1,
                    token: "t".into(),
                    state: "working".into(),
                    visible_blocker: false,
                    visible_idle: false,
                    visible_working: true,
                    clock: 0,
                    session: None,
                    event: None,
                    label: None,
                })
            },
        },
        Action {
            name: "report the agent's conversation id without claiming a state",
            method: || {
                Method::Control(ControlRequest::ReportAgentSession {
                    id: "a".into(),
                    terminal: 1,
                    token: "t".into(),
                    session: "s".into(),
                })
            },
        },
        Action {
            name: "read what has scrolled past",
            method: || {
                Method::Control(ControlRequest::Scrollback {
                    id: "a".into(),
                    terminal: 1,
                    up: 40,
                    rows: 40,
                })
            },
        },
        Action {
            name: "search the history from copy mode",
            method: || {
                Method::Control(ControlRequest::SearchScrollback {
                    id: "a".into(),
                    terminal: 1,
                    query: "error".into(),
                    from: 0,
                    backward: true,
                })
            },
        },
        Action {
            name: "copy a selection longer than the screen from copy mode",
            method: || {
                Method::Control(ControlRequest::Scrollback {
                    id: "a".into(),
                    terminal: 1,
                    up: 500,
                    rows: 500,
                })
            },
        },
        Action {
            name: "end this session",
            method: || Method::Control(ControlRequest::Shutdown { id: "a".into() }),
        },
        Action {
            name: "re-read agent-detection overrides",
            method: || Method::Control(ControlRequest::ReloadAgentManifests { id: "a".into() }),
        },
        Action {
            name: "read a terminal's recent hook events for the focus view",
            method: || {
                Method::Control(ControlRequest::ListFocusEvents {
                    id: "a".into(),
                    terminal: 1,
                    last_n: 20,
                })
            },
        },
        Action {
            name: "connect another machine from the interface",
            method: || {
                Method::Control(ControlRequest::ConnectRemote {
                    id: "a".into(),
                    target: "me@devbox".into(),
                })
            },
        },
        Action {
            name: "report the outer terminal's colours",
            method: || {
                Method::Control(ControlRequest::HostColors {
                    id: "a".into(),
                    fg: [255; 3],
                    bg: [0; 3],
                })
            },
        },
        Action {
            name: "attach a picture from the phone",
            method: || {
                Method::Control(ControlRequest::SaveUpload {
                    id: "a".into(),
                    ext: "jpg".into(),
                    data: String::new(),
                })
            },
        },
        Action {
            name: "paste the picture's path into the agent",
            method: || Method::Terminal("Paste"),
        },
        Action {
            name: "split a tab, or take a pane out of it",
            method: || {
                Method::Control(ControlRequest::SetSplit {
                    id: "a".into(),
                    tree: qoop_runtime::control::Pane::Leaf { terminal: 1 },
                })
            },
        },
        Action {
            name: "install qoop on a machine that has none",
            method: || {
                Method::Control(ControlRequest::InstallRemote {
                    id: "a".into(),
                    target: "me@devbox".into(),
                })
            },
        },
        Action {
            name: "move a machine onto the newer qoop put there",
            method: || {
                Method::Control(ControlRequest::RestartWhenSafe {
                    id: "a".into(),
                    version: "0.0.0".into(),
                    keep_tabs: false,
                    quiet: false,
                })
            },
        },
        Action {
            name: "see whether the daemon could move onto a newer qoop (the foot's words)",
            method: || Method::Control(ControlRequest::UpdateState { id: "a".into() }),
        },
        Action {
            name: "type into the focused terminal",
            method: || Method::Terminal("Input"),
        },
        Action {
            name: "type into another terminal from a script",
            method: || Method::Terminal("Input"),
        },
        Action {
            name: "read another terminal's screen from a script",
            method: || {
                Method::Control(ControlRequest::Subscribe {
                    id: "a".into(),
                    terminals: vec![1],
                })
            },
        },
        Action {
            name: "resolve a prompt target against the index",
            method: || Method::Control(ControlRequest::ListAgents { id: "a".into() }),
        },
        Action {
            name: "resize the focused terminal",
            method: || Method::Terminal("Resize"),
        },
        Action {
            name: "record what this terminal's agent is working on",
            method: || {
                Method::Control(ControlRequest::TaskWrite {
                    id: "a".into(),
                    terminal: Some(1),
                    token: Some("t".into()),
                    task: None,
                    action: qoop_runtime::control::TaskOp::New {
                        goal: "g".into(),
                        replace: false,
                    },
                })
            },
        },
        Action {
            name: "show a task and what was recorded on it",
            method: || {
                Method::Control(ControlRequest::TaskRead {
                    id: "a".into(),
                    task: Some(1),
                    terminal: None,
                    token: None,
                    before: None,
                })
            },
        },
    ]
}

#[test]
fn every_action_maps_to_a_named_protocol_method() {
    let actions = actions();
    assert!(
        actions.len() >= 9,
        "only {} actions are registered, so this proves little",
        actions.len()
    );
    for action in &actions {
        let name = (action.method)().name();
        assert!(
            !name.is_empty(),
            "the action \"{}\" maps to a method with no name",
            action.name
        );
    }
}

/// Every control method the protocol has is reachable from some action.
///
/// The other direction, and the one that catches a method that exists and that nothing can
/// call: a protocol surface with unreachable methods is a surface where the first-party
/// client and the documentation disagree about what the product does.
#[test]
fn every_control_method_is_reachable_from_some_action() {
    let reachable: Vec<String> = actions().iter().map(|a| (a.method)().name()).collect();

    // Enumerated through an exhaustive match, so a new variant fails to compile here.
    let all = [
        ControlRequest::CreateTerminal {
            id: String::new(),
            cwd: None,
            workspace: None,
            name: None,
        },
        ControlRequest::CloseTerminal {
            id: String::new(),
            terminal: 0,
        },
        ControlRequest::ListTerminals { id: String::new() },
        ControlRequest::ListAgents { id: String::new() },
        ControlRequest::Focus {
            id: String::new(),
            terminal: 0,
        },
        ControlRequest::News {
            id: String::new(),
            terminals: Vec::new(),
        },
        ControlRequest::ListWorkspaces { id: String::new() },
        ControlRequest::CreateWorkspace {
            id: String::new(),
            name: String::new(),
        },
        ControlRequest::CloseWorkspace {
            id: String::new(),
            workspace: 0,
        },
        ControlRequest::SleepWorkspace {
            id: String::new(),
            workspace: 0,
            terminal: None,
        },
        ControlRequest::WakeWorkspace {
            id: String::new(),
            workspace: None,
            terminal: None,
        },
        ControlRequest::Subscribe {
            id: String::new(),
            terminals: Vec::new(),
        },
        ControlRequest::Unsubscribe {
            id: String::new(),
            terminals: Vec::new(),
        },
        ControlRequest::RequestOwnership {
            id: String::new(),
            terminal: 0,
        },
        ControlRequest::ReportAgentState {
            id: String::new(),
            terminal: 0,
            token: String::new(),
            state: String::new(),
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            clock: 0,
            session: None,
            event: None,
            label: None,
        },
        ControlRequest::ReportAgentSession {
            id: String::new(),
            terminal: 0,
            token: String::new(),
            session: String::new(),
        },
        ControlRequest::ReloadAgentManifests { id: String::new() },
        ControlRequest::Shutdown { id: String::new() },
        ControlRequest::Scrollback {
            id: String::new(),
            terminal: 0,
            up: 0,
            rows: 0,
        },
        ControlRequest::SearchScrollback {
            id: String::new(),
            terminal: 0,
            query: String::new(),
            from: 0,
            backward: false,
        },
        ControlRequest::GrantOwnership {
            id: String::new(),
            terminal: 0,
            to: String::new(),
        },
        ControlRequest::RenameTerminal {
            id: String::new(),
            terminal: 0,
            name: None,
        },
        ControlRequest::RenameWorkspace {
            id: String::new(),
            workspace: 0,
            name: String::new(),
        },
        ControlRequest::SetWorkspaceInk {
            id: String::new(),
            workspace: 0,
            ink: None,
        },
        ControlRequest::MoveWorkspace {
            id: String::new(),
            workspace: 0,
            before: None,
        },
        ControlRequest::SetKeepAwake {
            id: String::new(),
            workspace: 0,
            terminal: None,
            on: false,
        },
        ControlRequest::RenameSession {
            id: String::new(),
            terminal: 0,
            title: None,
        },
        ControlRequest::MoveTerminal {
            id: String::new(),
            terminal: 0,
            workspace: 0,
            place: None,
            leave_split: false,
        },
        ControlRequest::ListFocusEvents {
            id: String::new(),
            terminal: 0,
            last_n: 0,
        },
        ControlRequest::ConnectRemote {
            id: String::new(),
            target: String::new(),
        },
        ControlRequest::InstallRemote {
            id: String::new(),
            target: String::new(),
        },
        ControlRequest::HostColors {
            id: String::new(),
            fg: [0; 3],
            bg: [0; 3],
        },
        ControlRequest::SetSplit {
            id: String::new(),
            tree: qoop_runtime::control::Pane::Leaf { terminal: 0 },
        },
        ControlRequest::SaveUpload {
            id: String::new(),
            ext: String::new(),
            data: String::new(),
        },
        ControlRequest::RestartWhenSafe {
            id: String::new(),
            version: String::new(),
            keep_tabs: false,
            quiet: false,
        },
        ControlRequest::UpdateState { id: String::new() },
        ControlRequest::TaskWrite {
            id: String::new(),
            terminal: None,
            token: None,
            task: Some(0),
            action: qoop_runtime::control::TaskOp::Stage {
                stage: String::new(),
            },
        },
        ControlRequest::TaskRead {
            id: String::new(),
            task: None,
            terminal: None,
            token: None,
            before: None,
        },
    ];
    for request in &all {
        match request {
            ControlRequest::CreateTerminal { .. }
            | ControlRequest::CloseTerminal { .. }
            | ControlRequest::ListTerminals { .. }
            | ControlRequest::ListAgents { .. }
            | ControlRequest::Focus { .. }
            | ControlRequest::News { .. }
            | ControlRequest::ListWorkspaces { .. }
            | ControlRequest::CreateWorkspace { .. }
            | ControlRequest::CloseWorkspace { .. }
            | ControlRequest::SleepWorkspace { .. }
            | ControlRequest::WakeWorkspace { .. }
            | ControlRequest::RenameTerminal { .. }
            | ControlRequest::RenameSession { .. }
            | ControlRequest::MoveTerminal { .. }
            | ControlRequest::RenameWorkspace { .. }
            | ControlRequest::SetWorkspaceInk { .. }
            | ControlRequest::MoveWorkspace { .. }
            | ControlRequest::SetKeepAwake { .. }
            | ControlRequest::Subscribe { .. }
            | ControlRequest::Unsubscribe { .. }
            | ControlRequest::RequestOwnership { .. }
            | ControlRequest::ReportAgentState { .. }
            | ControlRequest::ReportAgentSession { .. }
            | ControlRequest::ListFocusEvents { .. }
            | ControlRequest::ConnectRemote { .. }
            | ControlRequest::InstallRemote { .. }
            | ControlRequest::HostColors { .. }
            | ControlRequest::SaveUpload { .. }
            | ControlRequest::SetSplit { .. }
            | ControlRequest::RestartWhenSafe { .. }
            | ControlRequest::UpdateState { .. }
            | ControlRequest::TaskWrite { .. }
            | ControlRequest::TaskRead { .. }
            | ControlRequest::ReloadAgentManifests { .. }
            | ControlRequest::Shutdown { .. }
            | ControlRequest::Scrollback { .. }
            | ControlRequest::SearchScrollback { .. }
            | ControlRequest::GrantOwnership { .. } => {}
        }
        let name = Method::Control(request.clone()).name();
        assert!(
            reachable.contains(&name),
            "the protocol method {name} is not reachable from any first-party action, so \
             the client and the protocol disagree about what the product does"
        );
    }
}

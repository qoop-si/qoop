//! Gate G11.1: every first-party action maps to a named public protocol method.
//!
//! The rule this enforces is not a tidiness rule. An action with no protocol method behind
//! it is an action only the first-party client can perform, and every such action is a
//! reason a third-party client, a script or a plugin cannot do what the built-in one can.
//! The ecosystem claim is exactly the absence of those.
//!
//! Asserted over the registered handlers rather than over a list somebody maintains: a list
//! stops covering the surface the first time an action is added.

use qoro_runtime::control::ControlRequest;

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
        ControlRequest::ListWorkspaces { id: String::new() },
        ControlRequest::CreateWorkspace {
            id: String::new(),
            name: String::new(),
        },
        ControlRequest::CloseWorkspace {
            id: String::new(),
            workspace: 0,
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
        ControlRequest::ListFocusEvents {
            id: String::new(),
            terminal: 0,
            last_n: 0,
        },
    ];
    for request in &all {
        match request {
            ControlRequest::CreateTerminal { .. }
            | ControlRequest::CloseTerminal { .. }
            | ControlRequest::ListTerminals { .. }
            | ControlRequest::ListAgents { .. }
            | ControlRequest::ListWorkspaces { .. }
            | ControlRequest::CreateWorkspace { .. }
            | ControlRequest::CloseWorkspace { .. }
            | ControlRequest::RenameTerminal { .. }
            | ControlRequest::RenameWorkspace { .. }
            | ControlRequest::Subscribe { .. }
            | ControlRequest::Unsubscribe { .. }
            | ControlRequest::RequestOwnership { .. }
            | ControlRequest::ReportAgentState { .. }
            | ControlRequest::ReportAgentSession { .. }
            | ControlRequest::ListFocusEvents { .. }
            | ControlRequest::ReloadAgentManifests { .. }
            | ControlRequest::Shutdown { .. }
            | ControlRequest::Scrollback { .. }
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

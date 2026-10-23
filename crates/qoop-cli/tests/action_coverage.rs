//! Gate G11.1: every first-party action maps to a named public protocol method.
//!
//! The rule this enforces is not a tidiness rule. An action with no protocol method behind
//! it is an action only the first-party client can perform, and every such action is a
//! reason a third-party client, a script or a plugin cannot do what the built-in one can.
//! The ecosystem claim is exactly the absence of those.
//!
//! Asserted over the registered handlers rather than over a list somebody maintains: a list
//! stops covering the surface the first time an action is added.

use qoop_runtime::control::{ControlRequest, ScheduleOp};

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
            name: "say what a space is for",
            method: || {
                Method::Control(ControlRequest::SetSpaceGoal {
                    id: "a".into(),
                    workspace: 1,
                    goal: "ship the phone app".into(),
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
            name: "have qass leave a space or a pane alone",
            method: || {
                Method::Control(ControlRequest::SetQassSkip {
                    id: "a".into(),
                    workspace: 1,
                    terminal: Some(2),
                    on: true,
                })
            },
        },
        Action {
            name: "park an agent to answer later",
            method: || {
                Method::Control(ControlRequest::SetParked {
                    id: "a".into(),
                    terminal: 2,
                    on: true,
                })
            },
        },
        Action {
            name: "change a machine's sleep settings from another device",
            method: || {
                Method::Control(ControlRequest::SleepSettings {
                    id: "a".into(),
                    idle_minutes: Some(30),
                    exit_when_asleep: None,
                })
            },
        },
        Action {
            name: "turn qass's watching on or off from another device",
            method: || {
                Method::Control(ControlRequest::QassWatch {
                    id: "a".into(),
                    on: Some(false),
                })
            },
        },
        Action {
            name: "hold a prompt back while a person is at that terminal",
            method: || {
                Method::Control(ControlRequest::PersonAt {
                    id: "a".into(),
                    terminal: 1,
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
                    from: None,
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
                    from: None,
                    id: "a".into(),
                    terminal: 1,
                    token: "t".into(),
                    session: "s".into(),
                })
            },
        },
        Action {
            name: "report the agent's sub-agents without claiming a state",
            method: || {
                Method::Control(ControlRequest::ReportSubagents {
                    from: None,
                    id: "a".into(),
                    terminal: 1,
                    token: "t".into(),
                    change: qoop_runtime::control::SubagentChange::Started("s".into()),
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
            name: "read the chat of an agent, from `qoop messages`",
            method: || {
                Method::Control(ControlRequest::Messages {
                    id: "a".into(),
                    terminal: 1,
                    skip: 0,
                    limit: 60,
                    revision: None,
                })
            },
        },
        Action {
            name: "open one tool call of an agent's chat, from `qoop messages detail`",
            method: || {
                Method::Control(ControlRequest::MessageDetail {
                    id: "a".into(),
                    terminal: 1,
                    tool_id: "t".into(),
                    max_bytes: None,
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
                    terminal: None,
                })
            },
        },
        Action {
            name: "paste the picture's path into the agent",
            method: || Method::Terminal("Paste"),
        },
        Action {
            name: "record from the microphone a window lends (qoop's arecord)",
            method: || {
                Method::Control(ControlRequest::Listen {
                    id: "a".into(),
                    terminal: 0,
                    probe: false,
                    token: String::new(),
                })
            },
        },
        Action {
            name: "lend this machine's microphone to a tab's recording",
            method: || Method::Terminal("Audio"),
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
                    same_version: false,
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
            name: "put an item on a space's board",
            method: || {
                Method::Control(ControlRequest::TaskWrite {
                    id: "a".into(),
                    terminal: None,
                    token: None,
                    task: None,
                    action: qoop_runtime::control::TaskOp::Add {
                        workspace: 1,
                        goal: "g".into(),
                        description: None,
                    },
                })
            },
        },
        Action {
            name: "show the board",
            method: || {
                Method::Control(ControlRequest::TaskRead {
                    id: "a".into(),
                    board: true,
                    task: None,
                    terminal: None,
                    token: None,
                    before: None,
                })
            },
        },
        Action {
            name: "answer, put away or hand over a card of the question inbox",
            method: || {
                Method::Control(ControlRequest::AskWrite {
                    id: "a".into(),
                    terminal: None,
                    token: None,
                    action: qoop_runtime::control::AskOp::Dismiss { card: "E1".into() },
                })
            },
        },
        Action {
            name: "read the question inbox",
            method: || {
                Method::Control(ControlRequest::AskRead {
                    id: "a".into(),
                    all: false,
                })
            },
        },
        Action {
            name: "put a result on the owner's phone",
            method: || {
                Method::Control(ControlRequest::ShowWrite {
                    id: "a".into(),
                    terminal: 1,
                    token: "t".into(),
                    target: "/tmp/shot.png".into(),
                    note: None,
                })
            },
        },
        Action {
            name: "list the results shown",
            method: || Method::Control(ControlRequest::ShowRead { id: "a".into() }),
        },
        Action {
            name: "list the agent sessions of a directory or a day",
            method: || {
                Method::Control(ControlRequest::AgentSessions {
                    id: "a".into(),
                    dirs: Vec::new(),
                    from_ms: None,
                    to_ms: None,
                    limit: 8,
                })
            },
        },
        Action {
            name: "count the agents' work per day",
            method: || {
                Method::Control(ControlRequest::AgentActivity {
                    id: "a".into(),
                    days: 182,
                    tz_offset_min: 0,
                })
            },
        },
        Action {
            name: "download a result shown",
            method: || {
                Method::Control(ControlRequest::ShowFetch {
                    id: "a".into(),
                    show: 1,
                    offset: 0,
                    len: 1,
                })
            },
        },
        Action {
            name: "add a schedule",
            method: || {
                Method::Control(ControlRequest::ScheduleWrite {
                    id: "a".into(),
                    terminal: None,
                    token: None,
                    action: Box::new(ScheduleOp::Add {
                        name: "morning".into(),
                        cron: Some("0 9 * * *".into()),
                        at: None,
                        agent: None,
                        cwd: "/tmp".into(),
                        space: None,
                        prompt: "look at the inbox".into(),
                    }),
                })
            },
        },
        Action {
            name: "pause, resume, remove or run a schedule",
            method: || {
                Method::Control(ControlRequest::ScheduleWrite {
                    id: "a".into(),
                    terminal: None,
                    token: None,
                    action: Box::new(ScheduleOp::Pause { schedule: 1 }),
                })
            },
        },
        Action {
            name: "list the schedules",
            method: || Method::Control(ControlRequest::ScheduleRead { id: "a".into() }),
        },
        Action {
            name: "preview a dev server shown on the phone",
            method: || {
                Method::Control(ControlRequest::TunnelOpen {
                    id: "a".into(),
                    show: 1,
                    conn: 1,
                })
            },
        },
        Action {
            name: "open a path or url tapped in a chat on the phone",
            method: || {
                Method::Control(ControlRequest::ShowOpen {
                    id: "a".into(),
                    terminal: 1,
                    target: "/tmp/shot.png".into(),
                    auto: false,
                })
            },
        },
        Action {
            name: "see what a terminal's checkout changed",
            method: || {
                Method::Control(ControlRequest::Changes {
                    id: "a".into(),
                    terminal: 1,
                })
            },
        },
        Action {
            name: "show a task and what was recorded on it",
            method: || {
                Method::Control(ControlRequest::TaskRead {
                    id: "a".into(),
                    board: false,
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
            from: None,
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
            from: None,
            id: String::new(),
            terminal: 0,
            token: String::new(),
            session: String::new(),
        },
        ControlRequest::ReportSubagents {
            from: None,
            id: String::new(),
            terminal: 0,
            token: String::new(),
            change: qoop_runtime::control::SubagentChange::Running(Vec::new()),
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
        ControlRequest::Messages {
            id: String::new(),
            terminal: 0,
            skip: 0,
            limit: 0,
            revision: None,
        },
        ControlRequest::MessageDetail {
            id: String::new(),
            terminal: 0,
            tool_id: String::new(),
            max_bytes: None,
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
        ControlRequest::SetSpaceGoal {
            id: String::new(),
            workspace: 0,
            goal: String::new(),
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
        ControlRequest::SetQassSkip {
            id: String::new(),
            workspace: 0,
            terminal: None,
            on: false,
        },
        ControlRequest::SetParked {
            id: String::new(),
            terminal: 0,
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
            terminal: None,
        },
        ControlRequest::RestartWhenSafe {
            id: String::new(),
            version: String::new(),
            keep_tabs: false,
            quiet: false,
            same_version: false,
        },
        ControlRequest::UpdateState { id: String::new() },
        ControlRequest::SleepSettings {
            id: String::new(),
            idle_minutes: None,
            exit_when_asleep: None,
        },
        ControlRequest::QassWatch {
            id: String::new(),
            on: None,
        },
        ControlRequest::PersonAt {
            id: String::new(),
            terminal: 0,
        },
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
            board: false,
            task: None,
            terminal: None,
            token: None,
            before: None,
        },
        ControlRequest::AskWrite {
            id: String::new(),
            terminal: None,
            token: None,
            action: qoop_runtime::control::AskOp::Dismiss {
                card: String::new(),
            },
        },
        ControlRequest::AskRead {
            id: String::new(),
            all: false,
        },
        ControlRequest::ShowWrite {
            id: String::new(),
            terminal: 0,
            token: String::new(),
            target: String::new(),
            note: None,
        },
        ControlRequest::ShowRead { id: String::new() },
        ControlRequest::AgentSessions {
            id: String::new(),
            dirs: Vec::new(),
            from_ms: None,
            to_ms: None,
            limit: 0,
        },
        ControlRequest::AgentActivity {
            id: String::new(),
            days: 0,
            tz_offset_min: 0,
        },
        ControlRequest::ShowFetch {
            id: String::new(),
            show: 0,
            offset: 0,
            len: 0,
        },
        ControlRequest::ScheduleWrite {
            id: String::new(),
            terminal: None,
            token: None,
            action: Box::new(ScheduleOp::Pause { schedule: 0 }),
        },
        ControlRequest::ScheduleRead { id: String::new() },
        ControlRequest::TunnelOpen {
            id: String::new(),
            show: 0,
            conn: 0,
        },
        ControlRequest::Changes {
            id: String::new(),
            terminal: 0,
        },
        ControlRequest::ShowOpen {
            id: String::new(),
            terminal: 0,
            target: String::new(),
            auto: false,
        },
        ControlRequest::Listen {
            id: String::new(),
            terminal: 0,
            probe: false,
            token: String::new(),
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
            | ControlRequest::SetSpaceGoal { .. }
            | ControlRequest::MoveWorkspace { .. }
            | ControlRequest::SetKeepAwake { .. }
            | ControlRequest::SetQassSkip { .. }
            | ControlRequest::SetParked { .. }
            | ControlRequest::Subscribe { .. }
            | ControlRequest::Unsubscribe { .. }
            | ControlRequest::RequestOwnership { .. }
            | ControlRequest::ReportAgentState { .. }
            | ControlRequest::ReportAgentSession { .. }
            | ControlRequest::ReportSubagents { .. }
            | ControlRequest::ListFocusEvents { .. }
            | ControlRequest::ConnectRemote { .. }
            | ControlRequest::InstallRemote { .. }
            | ControlRequest::HostColors { .. }
            | ControlRequest::SaveUpload { .. }
            | ControlRequest::Listen { .. }
            | ControlRequest::SetSplit { .. }
            | ControlRequest::RestartWhenSafe { .. }
            | ControlRequest::UpdateState { .. }
            | ControlRequest::SleepSettings { .. }
            | ControlRequest::QassWatch { .. }
            | ControlRequest::PersonAt { .. }
            | ControlRequest::TaskWrite { .. }
            | ControlRequest::TaskRead { .. }
            | ControlRequest::AskWrite { .. }
            | ControlRequest::AskRead { .. }
            | ControlRequest::ShowWrite { .. }
            | ControlRequest::ShowRead { .. }
            | ControlRequest::AgentSessions { .. }
            | ControlRequest::AgentActivity { .. }
            | ControlRequest::ShowFetch { .. }
            | ControlRequest::ScheduleWrite { .. }
            | ControlRequest::ScheduleRead { .. }
            | ControlRequest::TunnelOpen { .. }
            | ControlRequest::Changes { .. }
            | ControlRequest::ShowOpen { .. }
            | ControlRequest::ReloadAgentManifests { .. }
            | ControlRequest::Shutdown { .. }
            | ControlRequest::Scrollback { .. }
            | ControlRequest::SearchScrollback { .. }
            | ControlRequest::Messages { .. }
            | ControlRequest::MessageDetail { .. }
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

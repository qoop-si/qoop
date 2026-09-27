//! The control channel: workspace and terminal lifecycle.
//!
//! Every request carries a client-chosen `id` and every response echoes it. That is what
//! makes a retry after an ambiguous failure safe to send: the client can tell the answer to
//! its own question from the answer to somebody else's, and a daemon that has already done
//! the work can say so instead of doing it twice.
//!
//! The shape is request and response rather than fire and forget because ADR-005's second
//! rule requires that a create be acknowledged only once it is durable. A message with no
//! reply cannot carry that promise.

use serde::{Deserialize, Serialize};

/// A terminal is addressed by the number that travels in a frame's `stream` field.
pub type TerminalStream = u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ControlRequest {
    /// Start a terminal. `cwd` is where the shell starts; `None` means the daemon's own.
    CreateTerminal {
        id: String,
        cwd: Option<String>,
        /// Which workspace to put it in. `None` means the one the daemon opened, which is
        /// what a client that does not know about workspaces means.
        #[serde(default)]
        workspace: Option<i64>,
        /// What a person called it, when they were asked.
        ///
        /// `None` means nobody chose one and the tab is named after whatever is running in
        /// it, which is the right answer for a terminal somebody opened in a hurry. A name
        /// outlives the program, so a tab called `release` stays `release` while it runs a
        /// shell, then cargo, then an agent.
        #[serde(default)]
        name: Option<String>,
    },
    /// Give a terminal a different name, or take its name away.
    ///
    /// A name and not a rename of the program: what is being changed is what a person
    /// calls this terminal, which is the thing that outlives whatever is running in it.
    RenameTerminal {
        id: String,
        terminal: TerminalStream,
        /// `None` gives the terminal back to its program's name.
        name: Option<String>,
    },
    /// Give a workspace a different name.
    /// Rows of a terminal's scrollback, so a person can read what has gone past.
    ///
    /// Read from what was written to disk rather than from the engine, which is what
    /// ADR-003 decided: the engine keeps a thousand rows of cells and the disk keeps rows
    /// of text, and a person scrolling back wants the second.
    Scrollback {
        id: String,
        terminal: u32,
        /// How far back the first row wanted is, counted from the newest row written.
        ///
        /// Counted from the newest and not from the oldest, because a client asking to
        /// see what just went past knows how far back it wants to look and does not know
        /// how much there is: asking from the oldest end would need a round trip to find
        /// out how many rows exist before the first useful question could be asked.
        up: usize,
        rows: usize,
    },
    /// End this daemon, and with it every terminal it owns.
    ///
    /// A request rather than a signal, so the daemon tears its terminals down the way
    /// ADR-014 says to rather than leaving whatever a `SIGKILL` leaves, and so the caller
    /// is told it was heard. What is durable survives: the next start of this session
    /// brings back the same spaces and tabs.
    Shutdown {
        id: String,
    },
    RenameWorkspace {
        id: String,
        workspace: i64,
        name: String,
    },
    /// End a terminal and everything it contains, per ADR-014.
    CloseTerminal {
        id: String,
        terminal: TerminalStream,
    },
    ListTerminals {
        id: String,
    },
    /// The workspaces this daemon holds, for a client that shows more than one.
    ListWorkspaces {
        id: String,
    },
    /// Start a workspace. Terminals are created in one, and a client that shows several
    /// needs a way to make the next one.
    CreateWorkspace {
        id: String,
        name: String,
    },
    /// Drop an empty workspace from the list.
    ///
    /// Only an empty one: a workspace that still holds terminals is refused, and so is
    /// the session's default, which is where a client that names no workspace lands.
    /// There is no "close and move its terminals" form: moving terminals between
    /// workspaces does not exist, so closing a full workspace would have to end them,
    /// and ending somebody's terminals as a side effect of tidying a list is not a
    /// trade this protocol makes.
    CloseWorkspace {
        id: String,
        workspace: i64,
    },
    /// The index a client draws: one row per terminal, with who is in it.
    ///
    /// Separate from `ListTerminals` rather than an extra field on it, because the two
    /// answer different questions and have different costs: a client that only needs the
    /// stream numbers should not pay for a read of every terminal's foreground process.
    ListAgents {
        id: String,
    },
    /// Receive renders for these terminals. Subscribing to a terminal twice is not an
    /// error and does not double the traffic: the set is a set.
    Subscribe {
        id: String,
        terminals: Vec<TerminalStream>,
    },
    Unsubscribe {
        id: String,
        terminals: Vec<TerminalStream>,
    },
    /// Ask the current owner to hand over the terminal's geometry.
    ///
    /// A request, not a claim. ADR-007 has no timeout that moves ownership, because a
    /// silent owner is not an absent one: the desktop client whose user stepped away still
    /// has the window the geometry belongs to. The owner answers, or nothing happens.
    RequestOwnership {
        id: String,
        terminal: TerminalStream,
    },
    /// The owner's answer. Sent by the owner, about a request it was told of.
    GrantOwnership {
        id: String,
        terminal: TerminalStream,
        to: String,
    },
    /// A hook or plugin inside the terminal reports the agent's state.
    ///
    /// An arrival, like output: frequent, not journalled, and never replayed. The
    /// daemon admits it through the same token and generation check as any other
    /// claim, then assigns its own sequence number. `clock` is the hook's wall
    /// time, advisory only: it is never copied into the observation's sequence,
    /// because a wall clock is not a counter and would let one source's numbers
    /// dwarf every other source's forever.
    ReportAgentState {
        id: String,
        terminal: TerminalStream,
        token: String,
        /// `starting`, `idle`, `working`, `blocked`, `done`, `failed`, `unknown`.
        state: String,
        #[serde(default)]
        visible_blocker: bool,
        #[serde(default)]
        visible_idle: bool,
        #[serde(default)]
        visible_working: bool,
        /// Wall-clock nanos from the hook. Advisory only, see above.
        #[serde(default)]
        clock: u64,
        /// The agent's own id for this conversation, when its hook knows one.
        ///
        /// What turns a restart into the same conversation rather than the newest one in
        /// the directory: two agents working in one repository both answer to "the newest
        /// one", and only one of them is right. Optional and defaulted, because most hooks
        /// do not report it and a daemon that predates the field ignores it.
        #[serde(default)]
        session: Option<String>,
        /// What happened, as a closed kind: `user`, `tool`, `blocked`, `done`.
        ///
        /// Display only: it never enters the belief, the push payload, or the
        /// event log. Absent or outside the closed set means "a state report
        /// with nothing to show", and the daemon records no event. Defaulted
        /// so reports from older hooks parse without it.
        #[serde(default)]
        event: Option<String>,
        /// One line saying what the event was: a tool name with its file-ish
        /// argument, or the head of the user's prompt. Cut to 120 characters
        /// by the daemon. Display only, like `event`: never journalled, never
        /// pushed, never observed. An event without a label is a kind with no
        /// words; a label without an event is refused by the CLI.
        #[serde(default)]
        label: Option<String>,
    },
    /// Re-read user agent-detection overrides from the config directory.
    ///
    /// Answered live from the files on disk, never journalled: replaying an
    /// old answer would lie about what the files say now.
    ReloadAgentManifests {
        id: String,
    },
    /// A hook or plugin inside the terminal reports the agent's own id for
    /// its conversation, without claiming a state.
    ///
    /// The session-only half of `ReportAgentState`: a third-party agent that
    /// knows its conversation but has no lifecycle event to map still needs
    /// to say which conversation a restart should come back to. Admitted
    /// through the same token and generation check, recorded in the same
    /// store, and answered with the same `AgentStateReported`. Never
    /// journalled and never an observation: the belief is untouched, so this
    /// cannot change what the index shows, only where a resume returns.
    ReportAgentSession {
        id: String,
        terminal: TerminalStream,
        token: String,
        /// The agent's own id for this conversation. Empty is refused: there
        /// is nothing to record, and silence must not clear what is there.
        session: String,
    },
    /// The focus events a terminal's hooks have reported.
    ///
    /// Answered from the daemon's memory, never from disk: events are
    /// arrivals like state reports, journalled nowhere and replayed never.
    /// `last_n` is capped by the daemon like scrollback rows.
    ListFocusEvents {
        id: String,
        terminal: TerminalStream,
        last_n: usize,
    },
    /// The sessions on another machine, reached over ssh. Answered by the interface's own
    /// connection to its daemons, never by a daemon: a daemon has no business opening ssh
    /// connections on a client's behalf.
    ListRemoteSessions {
        id: String,
        target: String,
    },
    /// Add a machine to the interface, and keep it: its spaces are listed from now on.
    /// Answered by the interface's connection, like `ListRemoteSessions`.
    ConnectRemote {
        id: String,
        target: String,
        session: String,
    },
    /// The foreground and background of the terminal a person is looking at, as the
    /// interface read them from it, so a program inside that asks for them (`OSC 10 ?`,
    /// `OSC 11 ?`) learns the real ones and picks a readable palette.
    ///
    /// Three bytes each and nothing else, so this can never carry anything to a child
    /// but an answer the engine formats itself. One pair for the whole daemon: the
    /// latest client to report wins, which with two windows on different backgrounds
    /// is the one the person most recently came back to.
    HostColors {
        id: String,
        fg: [u8; 3],
        bg: [u8; 3],
    },
}

impl ControlRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::CreateTerminal { id, .. }
            | Self::RenameTerminal { id, .. }
            | Self::RenameWorkspace { id, .. }
            | Self::Shutdown { id, .. }
            | Self::Scrollback { id, .. }
            | Self::CloseTerminal { id, .. }
            | Self::ListTerminals { id }
            | Self::ListAgents { id }
            | Self::ListWorkspaces { id }
            | Self::CreateWorkspace { id, .. }
            | Self::CloseWorkspace { id, .. }
            | Self::Subscribe { id, .. }
            | Self::Unsubscribe { id, .. }
            | Self::RequestOwnership { id, .. }
            | Self::ReportAgentState { id, .. }
            | Self::ReportAgentSession { id, .. }
            | Self::ListFocusEvents { id, .. }
            | Self::HostColors { id, .. }
            | Self::ReloadAgentManifests { id }
            | Self::ListRemoteSessions { id, .. }
            | Self::ConnectRemote { id, .. }
            | Self::GrantOwnership { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ControlResponse {
    TerminalCreated {
        id: String,
        terminal: TerminalStream,
    },
    TerminalClosed {
        id: String,
        terminal: TerminalStream,
    },
    Terminals {
        id: String,
        terminals: Vec<TerminalStream>,
    },
    Agents {
        id: String,
        agents: Vec<AgentRow>,
    },
    Workspaces {
        id: String,
        workspaces: Vec<WorkspaceRow>,
    },
    WorkspaceCreated {
        id: String,
        workspace: i64,
    },
    /// A workspace left the list. Only an empty one ever does; see `CloseWorkspace`.
    WorkspaceClosed {
        id: String,
        workspace: i64,
    },
    /// A terminal or a workspace now carries a different name.
    /// Rows of scrollback, and how many there are in total.
    ///
    /// The total is what lets a client turn a wheel into a position: without it a client
    /// would have to discover the end of the history by asking past it.
    Scrollback {
        id: String,
        terminal: u32,
        from: u64,
        total: u64,
        lines: Vec<String>,
    },
    /// The daemon heard, and is going. The count is what it is ending.
    Stopping {
        id: String,
        terminals: usize,
    },
    Renamed {
        id: String,
    },
    Subscribed {
        id: String,
        terminals: Vec<TerminalStream>,
    },
    /// The request reached the owner. Whether the owner answers is the owner's business,
    /// which is why this says "asked" and not "granted".
    OwnershipRequested {
        id: String,
        terminal: TerminalStream,
    },
    OwnershipGranted {
        id: String,
        terminal: TerminalStream,
        to: String,
    },
    /// A hook report was admitted and applied (or admitted and outranked: either
    /// way the daemon heard it and the answer is not a retryable failure).
    AgentStateReported {
        id: String,
        terminal: TerminalStream,
    },
    /// The override reload ran. `loaded` names manifest ids now serving
    /// override rules; `errors` names `file: reason` pairs that kept the
    /// previous rules. A missing override directory is an empty success.
    ManifestsReloaded {
        id: String,
        loaded: Vec<String>,
        errors: Vec<String>,
    },
    /// A refusal, with a reason a person can act on. Never a silent drop: a client waiting
    /// for a response it will never get is indistinguishable from a hung daemon.
    Failed {
        id: String,
        reason: String,
    },
    /// What `ListFocusEvents` asked for: the terminal's recent hook events,
    /// oldest first, at most as many as were asked for.
    FocusEvents {
        id: String,
        terminal: TerminalStream,
        events: Vec<FocusEvent>,
    },
    RemoteSessions {
        id: String,
        target: String,
        sessions: Vec<RemoteSession>,
    },
    /// The machine is added under `name`, and its spaces are on their way.
    RemoteAdded {
        id: String,
        name: String,
    },
    /// `HostColors` was taken: programs asking from now on get these.
    HostColorsNoted {
        id: String,
    },
}

/// One session on another machine, as `qoro ls` there describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSession {
    pub name: String,
    pub running: bool,
    pub tabs: usize,
}

/// One hook event, as the focus view draws it.
///
/// Plain strings on the wire, like `AgentRow`: a client that does not
/// recognise a kind renders the string it was given. The daemon only ever
/// stores the closed set (`user`, `tool`, `blocked`, `done`), so an unknown
/// kind here would be a daemon bug, not a version skew.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusEvent {
    pub kind: String,
    pub label: String,
    pub at_unix: i64,
}

/// One workspace, as a client lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: i64,
    pub name: String,
    /// How many terminals are open in it, so a client can show a workspace as busy or
    /// empty without asking about every terminal in it.
    pub terminals: usize,
}

/// One terminal, as an index shows it.
///
/// Strings on the wire and not the domain's own enums, deliberately. ADR-010 freezes the
/// shape of what crosses the socket, and a domain type that grows a variant would change
/// that shape without anyone deciding to. A client that does not recognise a state renders
/// the string it was given, which is the honest fallback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRow {
    pub terminal: TerminalStream,
    /// The workspace it belongs to, so a client can group by one without asking twice.
    /// No default. Zero is not a workspace, the store counts from one, and a row that
    /// arrived without this field would be filtered into a group that cannot exist: the tab
    /// bar would come up empty with terminals running in it, and the panel would name the
    /// space `0`. A missing field is a daemon that does not speak this protocol, which is
    /// worth failing on rather than drawing.
    pub workspace: i64,
    /// What a person called this terminal, when they were asked. Shown in place of the
    /// program on the tab, because a name somebody chose says more than the name of
    /// whatever happens to be running right now.
    #[serde(default)]
    pub name: Option<String>,
    /// The foreground program, when it can be read.
    pub program: Option<String>,
    /// What stands in the way, when it cannot. Never the wrapper's name as the program:
    /// a person shown `docker` concludes their agent is called docker.
    pub behind: Option<String>,
    /// `starting`, `idle`, `working`, `blocked`, `done`, `failed`, `uncertain`, `unknown`.
    pub state: String,
    /// Whether this terminal is past setup: `spawning`, `trust_required`,
    /// `booting`, or `ready`.
    ///
    /// A second dimension beside `state`, because "what the agent is doing"
    /// and "whether anything typed will reach a session" are different
    /// questions. A trust dialog reads as `blocked`, and blocked is refused
    /// by `prompt` either way, but an automation that cannot tell "session
    /// does not exist yet" from "agent is busy" cannot decide whether to wait
    /// for a person or for the agent. `ready` means past setup, not accepting
    /// tasks: a shell at its prompt is ready, and so is a finished agent.
    /// Defaulted, so a client from an older build reading a row from an older
    /// daemon sees `unknown` rather than failing to parse the row.
    #[serde(default = "unknown_readiness")]
    pub readiness: String,
    /// True when the program in this terminal is a coding agent rather than a shell or an
    /// ordinary command.
    ///
    /// Sent rather than worked out by the client, because the answer is a table of known
    /// agents that the daemon already consults and a client guessing from the name would be
    /// a second, quietly different copy of it. Defaulted, so a client from an older build
    /// reading a newer daemon sees "not an agent" rather than failing to parse the row.
    #[serde(default)]
    pub agent: bool,
    /// The agent's own id for its conversation, when it has reported one.
    /// Shortened for display by the client. Defaulted so rows from an older
    /// daemon parse without it.
    #[serde(default)]
    pub agent_session: Option<String>,
    /// The agent's own title for its conversation, when the daemon could read
    /// it out of the agent's own storage. Opencode keeps titles in its
    /// database next to the session id the hook already reports, so the panel
    /// names the conversation the way the agent does rather than the way the
    /// tab does. Re-resolved while the conversation runs, so a rename arrives
    /// on its own. Defaulted like every younger field.
    #[serde(default)]
    pub agent_title: Option<String>,
    /// Bumped when the process in the terminal is replaced, so a client can tell a new
    /// agent from a new state.
    pub generation: Option<u64>,
    /// Which source holds the belief: `Hook`, `ScreenHeuristic`, `ProcessIdentity`.
    ///
    /// What `qoro agents explain` shows as the authority behind the state. Absent
    /// when no observation has been accepted yet, and absent on rows from a
    /// daemon that predates the field.
    #[serde(default)]
    pub source: Option<String>,
    /// The screen rule that last matched in this generation, if any.
    ///
    /// The rule's name, never the screen it matched: a screen is somebody's
    /// source code and their client's name. `known_agent_idle_fallback`
    /// means no rule matched and the known-agent fallback read idle. Absent
    /// when the screen has not been evaluated in this generation, and absent
    /// on rows from a daemon that predates the field.
    #[serde(default)]
    pub rule: Option<String>,
    /// The manifest that produced the rule, by id.
    ///
    /// Absent together with `rule`. Absent on rows from a daemon that
    /// predates the field.
    #[serde(default)]
    pub manifest: Option<String>,
    /// When the agent in this terminal last did something, as unix seconds.
    ///
    /// Stamped by the daemon at arrival, from its own clock, on every
    /// accepted belief change and generation bump. Never copied from a
    /// hook's advisory clock. Absent until the first accepted observation
    /// (including after a restart, which must not paint a fleet as fresh),
    /// and absent on rows from a daemon that predates the field. What the
    /// client's idle tiers are computed from.
    #[serde(default)]
    pub last_active_at_unix: Option<i64>,
    /// When this terminal was slept by the idle reaper, as unix seconds on
    /// the daemon's own clock. Present only on rows with `state:
    /// "sleeping"`: a tombstone kept so a slept tab stays visible instead
    /// of vanishing. The process is gone; the agent's own conversation
    /// survives in the agent's own store. Absent on every live row and on
    /// rows from a daemon that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slept_at_unix: Option<i64>,
    /// Where this terminal is, last known, as an absolute path.
    ///
    /// Best effort with a documented bound: the per-terminal task writes it
    /// to the registry whenever its throttled read sees a change, so it can
    /// lag the shell by the containment scan interval plus spawn lag. Unknown is `None`,
    /// never an empty string; a client treats missing and blank alike.
    /// Absent on rows from a daemon that predates the field. What the
    /// client's place line disambiguates with.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// The readiness an older daemon's row carries when the field is absent.
///
/// `unknown` and not `ready`: a row that predates readiness says nothing
/// about setup, and reading silence as ready would type over a trust screen.
fn unknown_readiness() -> String {
    "unknown".to_string()
}

/// Whether a terminal is past setup, from what the index already knows.
///
/// The gate first: a trust dialog on screen means no session exists yet no
/// matter what else the row says. Then identity: nothing observed yet means
/// the process may not have printed its first byte. Then starting. Everything
/// else is past setup, including a shell at its prompt and a finished agent:
/// `ready` answers "will typed bytes reach a session", never "will the agent
/// do the task".
pub fn readiness_of(
    program: Option<&str>,
    behind: Option<&str>,
    state: &str,
    setup_gate: bool,
) -> &'static str {
    if setup_gate {
        "trust_required"
    } else if program.is_none() && behind.is_none() {
        "spawning"
    } else if state == "starting" {
        "booting"
    } else {
        "ready"
    }
}

impl ControlResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::TerminalCreated { id, .. }
            | Self::TerminalClosed { id, .. }
            | Self::Terminals { id, .. }
            | Self::Agents { id, .. }
            | Self::Workspaces { id, .. }
            | Self::WorkspaceCreated { id, .. }
            | Self::WorkspaceClosed { id, .. }
            | Self::Renamed { id, .. }
            | Self::Stopping { id, .. }
            | Self::Scrollback { id, .. }
            | Self::Subscribed { id, .. }
            | Self::OwnershipRequested { id, .. }
            | Self::OwnershipGranted { id, .. }
            | Self::AgentStateReported { id, .. }
            | Self::ManifestsReloaded { id, .. }
            | Self::FocusEvents { id, .. }
            | Self::RemoteSessions { id, .. }
            | Self::RemoteAdded { id, .. }
            | Self::HostColorsNoted { id }
            | Self::Failed { id, .. } => id,
        }
    }
}

/// The most focus events one terminal keeps.
///
/// A bound, not a target: the list is a glance, and an unbounded per-terminal
/// log in daemon memory grows with how long the session has been open.
pub const MAX_FOCUS_EVENTS: usize = 50;

/// The most characters of a focus label the daemon keeps.
///
/// Characters, not bytes: a label is cut on a char boundary, and a cut in
/// the middle of a multi-byte character would paint garbage.
pub const MAX_FOCUS_LABEL_CHARS: usize = 120;

/// Whether a reported focus kind names a real event.
///
/// The set is closed on purpose, like the push reasons: a free string is
/// where a second protocol grows. Anything else means "a state report with
/// nothing to show", never a failure: older hooks predate the field.
pub fn parse_focus_event(kind: &str) -> bool {
    matches!(kind, "user" | "tool" | "blocked" | "done")
}

/// Cut a focus label to [`MAX_FOCUS_LABEL_CHARS`] characters.
///
/// Runs of whitespace collapse to one space first: a label travels on one
/// row, and a multi-line prompt head would otherwise break the row it is
/// drawn on.
pub fn truncate_focus_label(label: &str) -> String {
    let single: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= MAX_FOCUS_LABEL_CHARS {
        return single;
    }
    single
        .chars()
        .take(MAX_FOCUS_LABEL_CHARS)
        .collect::<String>()
}

/// The state a hook may report, as the daemon reads it.
///
/// The same seven a manifest rule may declare. `uncertain` is deliberately
/// absent: it is a conclusion the precedence layer draws when two sources
/// disagree, and a single report cannot disagree with itself.
pub fn parse_reported_state(name: &str) -> Option<qoro_domain::AgentRuntimeState> {
    use qoro_domain::AgentRuntimeState as S;
    Some(match name {
        "starting" => S::Starting,
        "idle" => S::Idle,
        "working" => S::Working,
        "blocked" => S::Blocked,
        "done" => S::Done,
        "failed" => S::Failed,
        "unknown" => S::Unknown,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_names_setup_before_identity_before_starting() {
        // The gate wins over everything: no session exists while it stands.
        assert_eq!(
            readiness_of(Some("codex"), None, "blocked", true),
            "trust_required"
        );
        // Nothing observed yet: the process may not have printed its first byte.
        assert_eq!(readiness_of(None, None, "unknown", false), "spawning");
        assert_eq!(
            readiness_of(None, None, "starting", false),
            "spawning",
            "no identity outranks the state string"
        );
        // A known program still coming up.
        assert_eq!(
            readiness_of(Some("claude"), None, "starting", false),
            "booting"
        );
        // Past setup, whatever the state: a shell at its prompt, a busy
        // agent, a mid-task approval, a finished one.
        for state in ["idle", "working", "blocked", "done", "failed", "unknown"] {
            assert_eq!(
                readiness_of(Some("sh"), None, state, false),
                "ready",
                "{state}"
            );
        }
        assert_eq!(
            readiness_of(None, Some("docker"), "unknown", false),
            "ready",
            "a wrapper hides the program, not the session"
        );
    }

    #[test]
    fn a_row_from_an_older_daemon_reads_unknown_rather_than_ready() {
        // No `readiness` key: the row predates the field, and silence must
        // not read as permission to type over a trust screen.
        let row: AgentRow =
            serde_json::from_str(r#"{"terminal":1,"workspace":1,"state":"blocked"}"#)
                .expect("decode");
        assert_eq!(row.readiness, "unknown");
        assert_eq!(row.agent_session, None);
    }

    #[test]
    fn an_explanation_from_an_older_daemon_is_absent_rather_than_wrong() {
        // No `source`, `rule` or `manifest` keys: the row predates the
        // fields, and silence must read as "nothing to say" rather than as a
        // rule that never matched.
        let row: AgentRow =
            serde_json::from_str(r#"{"terminal":1,"workspace":1,"state":"blocked"}"#)
                .expect("decode");
        assert_eq!(row.source, None);
        assert_eq!(row.rule, None);
        assert_eq!(row.manifest, None);
        // And a row that carries them keeps them across the wire.
        let explained: AgentRow = serde_json::from_str(
            r#"{"terminal":2,"workspace":1,"state":"blocked","source":"ScreenHeuristic","rule":"needs_approval","manifest":"claude"}"#,
        )
        .expect("decode");
        assert_eq!(explained.source.as_deref(), Some("ScreenHeuristic"));
        assert_eq!(explained.rule.as_deref(), Some("needs_approval"));
        assert_eq!(explained.manifest.as_deref(), Some("claude"));
    }

    /// A sleeping row carries when it slept; a row from an older daemon
    /// parses with it absent, and an older client ignores what it does not
    /// know (no `deny_unknown_fields` on the struct).
    #[test]
    fn a_sleeping_row_carries_when_it_slept_both_ways() {
        let old: AgentRow =
            serde_json::from_str(r#"{"terminal":1,"workspace":1,"state":"sleeping"}"#)
                .expect("decode");
        assert_eq!(old.slept_at_unix, None);
        let new: AgentRow = serde_json::from_str(
            r#"{"terminal":2,"workspace":1,"state":"sleeping","slept_at_unix":1750000000,"future_key":"ignored"}"#,
        )
        .expect("decode");
        assert_eq!(new.slept_at_unix, Some(1_750_000_000));
        // Skipped on the wire when absent: live rows say nothing new.
        let live = AgentRow {
            terminal: 3,
            workspace: 1,
            name: None,
            program: Some("zsh".into()),
            behind: None,
            state: "idle".into(),
            readiness: "ready".into(),
            agent: false,
            agent_session: None,
            agent_title: None,
            generation: None,
            source: None,
            rule: None,
            manifest: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
        };
        let encoded = serde_json::to_string(&live).expect("encode");
        assert!(!encoded.contains("slept_at_unix"), "{encoded}");
    }

    /// G8: activity and place fields cross old daemons both ways. A row from
    /// an older daemon parses with both absent; a row that carries them keeps
    /// them. `future_key` stands in for the next version's field: no
    /// `deny_unknown_fields` anywhere on the struct, so an older client
    /// reading a newer daemon's rows ignores what it does not know.
    #[test]
    fn activity_and_place_fields_parse_both_ways() {
        let old: AgentRow =
            serde_json::from_str(r#"{"terminal":1,"workspace":1,"state":"idle"}"#).expect("decode");
        assert_eq!(old.last_active_at_unix, None);
        assert_eq!(old.cwd, None);
        let new: AgentRow = serde_json::from_str(
            r#"{"terminal":2,"workspace":1,"state":"idle","last_active_at_unix":1750000000,"cwd":"/tmp/work","future_key":"ignored"}"#,
        )
        .expect("decode");
        assert_eq!(new.last_active_at_unix, Some(1_750_000_000));
        assert_eq!(new.cwd.as_deref(), Some("/tmp/work"));
    }

    #[test]
    fn a_request_and_its_response_carry_the_same_id() {
        let request = ControlRequest::CreateTerminal {
            id: "r1".into(),
            cwd: None,
            workspace: None,
            name: None,
        };
        let response = ControlResponse::TerminalCreated {
            id: request.request_id().to_string(),
            terminal: 7,
        };
        assert_eq!(request.request_id(), response.request_id());
    }

    #[test]
    fn the_wire_form_round_trips() {
        for request in [
            ControlRequest::CreateTerminal {
                id: "a".into(),
                cwd: Some("/tmp".into()),
                workspace: Some(2),
                name: None,
            },
            ControlRequest::CloseTerminal {
                id: "b".into(),
                terminal: 3,
            },
            ControlRequest::Subscribe {
                id: "c".into(),
                terminals: vec![1, 2, 3],
            },
            ControlRequest::ReportAgentState {
                id: "d".into(),
                terminal: 3,
                token: "qoro-3-abc".into(),
                state: "working".into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: true,
                clock: 1758000000000000000,
                session: None,
                event: Some("tool".into()),
                label: Some("Read foo.rs".into()),
            },
            ControlRequest::ReloadAgentManifests { id: "e".into() },
        ] {
            let encoded = serde_json::to_vec(&request).expect("encode");
            let decoded: ControlRequest = serde_json::from_slice(&encoded).expect("decode");
            assert_eq!(request, decoded);
        }
    }

    #[test]
    fn a_hook_report_carries_its_id_and_an_old_client_omits_the_new_fields() {
        // New fields are defaulted, so a hook built against an older schema still
        // parses: it just claims no visible flags, no clock, and no focus event.
        let minimal =
            br#"{"op":"ReportAgentState","id":"x","terminal":1,"token":"t","state":"idle"}"#;
        let request: ControlRequest = serde_json::from_slice(minimal).expect("decode");
        assert_eq!(
            request,
            ControlRequest::ReportAgentState {
                id: "x".into(),
                terminal: 1,
                token: "t".into(),
                state: "idle".into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
                clock: 0,
                session: None,
                event: None,
                label: None,
            }
        );
    }

    #[test]
    fn only_the_four_focus_kinds_count_as_an_event() {
        for kind in ["user", "tool", "blocked", "done"] {
            assert!(parse_focus_event(kind), "{kind}");
        }
        // Anything else is a state report with nothing to show, never a failure.
        for kind in ["", "working", "idle", "started", "TOOL", "note"] {
            assert!(!parse_focus_event(kind), "{kind}");
        }
    }

    #[test]
    fn a_focus_label_is_single_line_and_cut_on_a_char_boundary() {
        assert_eq!(truncate_focus_label("Read foo.rs"), "Read foo.rs");
        assert_eq!(
            truncate_focus_label("first line\nsecond line"),
            "first line second line"
        );
        let long = "x".repeat(MAX_FOCUS_LABEL_CHARS + 40);
        let cut = truncate_focus_label(&long);
        assert_eq!(cut.chars().count(), MAX_FOCUS_LABEL_CHARS);
        // Multibyte survives the cut: no half character at the edge.
        let wide = "ö".repeat(MAX_FOCUS_LABEL_CHARS + 40);
        let cut = truncate_focus_label(&wide);
        assert_eq!(cut.chars().count(), MAX_FOCUS_LABEL_CHARS);
        assert!(cut.is_char_boundary(cut.len()));
    }

    #[test]
    fn a_focus_event_list_round_trips_with_its_id() {
        let request = ControlRequest::ListFocusEvents {
            id: "f1".into(),
            terminal: 2,
            last_n: 20,
        };
        let encoded = serde_json::to_vec(&request).expect("encode");
        let decoded: ControlRequest = serde_json::from_slice(&encoded).expect("decode");
        assert_eq!(request, decoded);
        let response = ControlResponse::FocusEvents {
            id: request.request_id().to_string(),
            terminal: 2,
            events: vec![FocusEvent {
                kind: "tool".into(),
                label: "Read foo.rs".into(),
                at_unix: 100,
            }],
        };
        assert_eq!(request.request_id(), response.request_id());
        let encoded = serde_json::to_vec(&response).expect("encode");
        let decoded: ControlResponse = serde_json::from_slice(&encoded).expect("decode");
        assert_eq!(response, decoded);
    }

    #[test]
    fn only_the_seven_reportable_states_parse_and_uncertain_does_not() {
        use qoro_domain::AgentRuntimeState as S;
        for (name, state) in [
            ("starting", S::Starting),
            ("idle", S::Idle),
            ("working", S::Working),
            ("blocked", S::Blocked),
            ("done", S::Done),
            ("failed", S::Failed),
            ("unknown", S::Unknown),
        ] {
            assert_eq!(parse_reported_state(name), Some(state), "{name}");
        }
        assert_eq!(parse_reported_state("uncertain"), None);
        assert_eq!(parse_reported_state("busy"), None);
        assert_eq!(parse_reported_state(""), None);
    }

    #[test]
    fn an_unknown_operation_is_rejected_rather_than_defaulted() {
        let bad = br#"{"op":"DestroyEverything","id":"x"}"#;
        assert!(serde_json::from_slice::<ControlRequest>(bad).is_err());
    }
}

/// What the daemon says without being asked.
///
/// On its own channel, because a notification is not an answer: a client waiting for the
/// response to its own request must not have to filter somebody else's news out of the
/// same lane, and a client that ignores events entirely still works.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum EventMessage {
    /// The process in this terminal, or what is believed about it, is not what it was.
    ///
    /// Deliberately not the new state: a notification that carries state is a second source
    /// of truth that can arrive out of order with the first. This says where to look, and
    /// `ListAgents` says what is there.
    AgentChanged { terminal: TerminalStream },
    /// Some news for this connection was dropped before it arrived. Re-read the whole
    /// index rather than waiting for the next change: the missed change may be the last
    /// one for a while, and waiting would leave the client behind silently.
    ///
    /// An older client that does not know this variant ignores it (the event lane
    /// decodes leniently by design), which degrades to the old behaviour of waiting
    /// for the next change rather than breaking the connection.
    Resync,
}

#[cfg(test)]
mod version_message_tests {
    /// An old daemon says so, rather than reporting a serde error at a person.
    ///
    /// Measured against the real thing: a daemon two days old answered a rename with
    /// `unknown variant RenameWorkspace, expected one of ...`, which reads as a broken
    /// client. The situation is ordinary, because a daemon keeps running across an
    /// upgrade, and the fix is one command.
    #[test]
    fn a_daemon_older_than_its_client_says_which_one_is_old() {
        let said = crate::daemon::unparsable(
            "unknown variant `RenameWorkspace`, expected one of `CreateTerminal`, `Subscribe`",
        );
        assert!(said.contains("older"), "{said}");
        assert!(
            said.contains("stop the session"),
            "the fix has to be in the message: {said}"
        );
        assert!(
            said.contains("comes back"),
            "and it has to say the work is not lost, or nobody will run it: {said}"
        );
        // Anything else is still reported as what it is.
        let other = crate::daemon::unparsable("expected value at line 1 column 1");
        assert!(other.contains("malformed"), "{other}");
        assert!(!other.contains("older"), "{other}");
    }
}

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

use qoop_parsed::redact;
use serde::{Deserialize, Serialize};

/// A terminal is addressed by the number that travels in a frame's `stream` field.
pub type TerminalStream = u32;

/// Where in a space's order a moved tab goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// Just before this tab.
    Before(TerminalStream),
    /// After every live tab of the space.
    End,
}

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
    /// Call the conversation in a terminal something else here, or with `None` give it
    /// back the title its agent gave it.
    ///
    /// Keyed by the agent's own id for the conversation, so the name follows it through a
    /// sleep and a restart. Never written into the agent's storage: it is what this
    /// machine calls the conversation, and every client reads it off `AgentRow::agent_title`.
    /// Refused for a terminal whose agent has not said which conversation it is in.
    RenameSession {
        id: String,
        terminal: TerminalStream,
        title: Option<String>,
    },
    /// Put a terminal in a workspace, at `place` in its order.
    ///
    /// Into another workspace a pane leaves the split it was in and the rest of that split
    /// stays. Within its own workspace it is a reorder: the tab moves with its split, and
    /// only a split's first pane can move it. Refused for a sleeping space on either side: a
    /// sleeping tab has nothing running to move, and one moved into a sleep would be running
    /// where nothing may run.
    ///
    /// Without a `place`, the request older clients send: the end of another workspace's
    /// tabs, and nothing at all in its own.
    MoveTerminal {
        id: String,
        terminal: TerminalStream,
        workspace: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        place: Option<Place>,
        /// Within its own workspace, a pane taken out of its split for a tab of its own at
        /// `place`, whichever pane of the split it is. Absent, a split's first pane moves
        /// the whole split.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        leave_split: bool,
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
    /// A page of the chat of the agent in a terminal, parsed from the agent's own session
    /// file by the daemon that owns the disk: the newest `limit` items older than the
    /// newest `skip`, in order. `revision` (always below 2^53, so a JavaScript number holds it
    /// exactly) is the one the client's last page carried; when
    /// it is not the current one the file has changed, `skip` means nothing any more, and
    /// the answer is the newest page with `reset` set.
    ///
    /// Answered for any client and any terminal by number, like `Scrollback`: whoever may
    /// read a terminal's screen may read its chat. Refused with a short reason code in
    /// `Failed`: `no_session` (the agent has not said which session it is in, or its file
    /// is not there yet), `not_agent` (a shell or any other program), `too_large` (a
    /// session the view does not read, use the terminal), `unavailable` (no such terminal,
    /// or the disk did not answer in time). An older daemon answers `unknown variant`.
    Messages {
        id: String,
        terminal: TerminalStream,
        skip: u32,
        limit: u32,
        #[serde(default)]
        revision: Option<u64>,
    },
    /// The full input and output of one tool call listed by `Messages`, each cut at
    /// `max_bytes` (64 KiB when absent, never more than 1 MiB), with secrets masked. Same
    /// authorization and reason codes as `Messages`; `no_session` also when the call is
    /// not in the file.
    MessageDetail {
        id: String,
        terminal: TerminalStream,
        tool_id: String,
        #[serde(default)]
        max_bytes: Option<u32>,
    },
    /// Look for text in what has scrolled past, one bounded step at a time.
    ///
    /// A step and not a whole search, because a week of history is millions of rows and a
    /// scan of all of them is seconds of disk: the daemon looks until a time and a byte
    /// budget run out, then answers with where to carry on, so the person sees progress and
    /// a keystroke is never queued behind a scan. The rows are the ones `Scrollback` counts,
    /// from the oldest written.
    SearchScrollback {
        id: String,
        terminal: TerminalStream,
        query: String,
        /// The first row looked at. Clamped to the newest row there is.
        #[serde(default)]
        from: u64,
        /// Toward older rows rather than newer ones.
        #[serde(default)]
        backward: bool,
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
    /// Pick a workspace's colour: one of the six space inks, or `None` for the one its id
    /// gives it. Answered `Renamed`, like the rename beside it, and announced to nobody
    /// either: another client sees it on its next list.
    SetWorkspaceInk {
        id: String,
        workspace: i64,
        ink: Option<u8>,
    },
    /// Say what the owner wants from the work in a workspace, one line; an empty text
    /// clears it. Answered `Renamed`, like the colour, and announced to nobody: clients see
    /// it on their next list.
    SetSpaceGoal {
        id: String,
        workspace: i64,
        goal: String,
    },
    /// Put a workspace just before `before` in the list, or last. Answered `Renamed`, like
    /// the colour; the list is read again by whoever asked, and by the others at their next
    /// listing.
    MoveWorkspace {
        id: String,
        workspace: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<i64>,
    },
    /// Keep a space awake, or with `terminal` one pane of it: idle sleep passes it by. A
    /// sleep a person asks for still sleeps it. Answered `Renamed`, like the colour; a pane's
    /// change is announced, so every client redraws its row.
    SetKeepAwake {
        id: String,
        workspace: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<u32>,
        on: bool,
    },
    /// Have qass leave a space alone, or with `terminal` one pane of it: the watcher says
    /// nothing about it and qass's own commands refuse it. Refused for the qass space and
    /// qass's own tab. Answered `Renamed`, like keep awake; the change is announced, so every
    /// client and the phone redraw the rows.
    SetQassSkip {
        id: String,
        workspace: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<u32>,
        on: bool,
    },
    /// Park a terminal's agent to answer later, or take it back: a person read its reply and
    /// set it aside. Refused while the agent works. Answered `Renamed`, like keep awake; the
    /// change is announced, so every client and the phone redraw the row.
    SetParked {
        id: String,
        terminal: u32,
        on: bool,
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
    /// Put a tab to sleep, or every tab of a workspace: what sleeps ends, its rows stay,
    /// and nothing of it runs until it wakes.
    ///
    /// A tab sleeps whole, every pane of its split with it, or not at all: one that could
    /// not come back as it is (a command running, an agent busy or not yet telling its
    /// conversation, a job in the background) stays awake. With `terminal`, that tab (any
    /// of its panes names it), which must be in `workspace`. Without, every awake tab of
    /// the workspace, each on its own: the answer says which stayed awake and why, and a
    /// `Failed` means none slept.
    SleepWorkspace {
        id: String,
        workspace: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalStream>,
    },
    /// Bring sleeping tabs back: the same numbers, names and directories, and each agent
    /// resumed into its own conversation. With `terminal`, that tab; else every sleeping
    /// tab of `workspace`; with neither, the tab that went to sleep last.
    WakeWorkspace {
        id: String,
        #[serde(default)]
        workspace: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalStream>,
    },
    /// The index a client draws: one row per terminal, with who is in it.
    ///
    /// Separate from `ListTerminals` rather than an extra field on it, because the two
    /// answer different questions and have different costs: a client that only needs the
    /// stream numbers should not pay for a read of every terminal's foreground process.
    ListAgents {
        id: String,
    },
    /// Show this terminal in every attached interface, the way a click on its row does.
    /// What a notch or another launcher sends so that picking an agent there lands on it.
    Focus {
        id: String,
        terminal: TerminalStream,
    },
    /// The terminals this interface counts as news right now, replacing what it said
    /// before. Held for the connection's life; see `AgentRow::news`.
    News {
        id: String,
        terminals: Vec<TerminalStream>,
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
        /// Display only: it never enters the belief, the push payload or the
        /// store. Absent or outside the closed set means "a state report with
        /// nothing to show", and the daemon records nothing. Defaulted so
        /// reports from older hooks parse without it.
        #[serde(default)]
        event: Option<String>,
        /// One line saying what the event was: a tool name with its file-ish
        /// argument, or the head of the user's prompt. Cut to 120 characters
        /// by the daemon. Display only, like `event`: never journalled, never
        /// pushed, never observed. An event without a label is a kind with no
        /// words; a label without an event is refused by the CLI.
        #[serde(default)]
        label: Option<String>,
        /// The agent whose hook sent this (`claude`, `codex`, ...). A report from another
        /// agent than the terminal's own is refused: an agent the terminal's agent started (Claude
        /// running `codex exec` as a reviewer) inherits the terminal and its token, and would
        /// otherwise speak for it. Absent (an older hook) is admitted as before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
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
        /// The agent whose hook sent this, as in `ReportAgentState`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// A hook or plugin inside the terminal says a sub-agent started or ended, or which ones
    /// run now. Never a state report and never journalled: the agent's own state is left
    /// alone, and the row only counts them. Admitted through the same token check as
    /// `ReportAgentSession`.
    ReportSubagents {
        id: String,
        terminal: TerminalStream,
        token: String,
        change: SubagentChange,
        /// The agent whose hook sent this, as in `ReportAgentState`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
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
    /// Add a machine to the interface, and keep it: its spaces are listed from now on.
    ///
    /// Answered by the interface's own connection to its daemons, never by a daemon: a
    /// daemon has no business opening ssh connections on a client's behalf. A machine with
    /// no qoop is answered `RemoteMissing` rather than added.
    ConnectRemote {
        id: String,
        target: String,
    },
    /// Put this version of qoop on a machine that has none, into `~/.local/bin` over ssh.
    /// Answered by the interface's connection, like `ConnectRemote`; it never replaces a
    /// qoop that is already there.
    InstallRemote {
        id: String,
        target: String,
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
    /// Keep a file sent from another device (a picture attached on the phone) for a
    /// program here to read, and answer where it is.
    ///
    /// Written 0600 into the session's own `uploads/` under a name the daemon makes, so
    /// nothing the sender says reaches the path. Never typed anywhere: the sender pastes
    /// the path on the terminal channel, with the authority of any keystroke.
    SaveUpload {
        id: String,
        /// The picture's kind, as its extension (`daemon::UPLOAD_KINDS`).
        ext: String,
        /// The bytes, as standard base64.
        data: String,
        /// The tab the picture is for. Only the interface's mux reads it, to send a picture
        /// for a remote tab to the daemon that runs the tab; a daemon ignores it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<u32>,
    },
    /// Borrow a microphone for a program in `terminal` (qoop's `arecord`): answered
    /// `Listening`, then `Heard` with what it hears until this connection goes. A `probe`
    /// only asks whether an interface that lends one is attached. See `microphone`.
    Listen {
        id: String,
        terminal: u32,
        #[serde(default)]
        probe: bool,
        /// `terminal`'s own token (`QOOP_TOKEN`): only a program in that tab may ask.
        #[serde(default)]
        token: String,
    },
    /// Record one tab's split tree, so a restart or a new client brings the panes back
    /// as one tab rather than as a tab each.
    ///
    /// The tree replaces every stored tree that shares a terminal with it, which is
    /// what moves a tree under a new first pane and what clears one: a single `Leaf`
    /// stores nothing and removes the tree that held it. Answered once durable.
    SetSplit {
        id: String,
        tree: Pane,
    },
    /// Move onto the qoop now on disk, `version`, at the first moment nothing here is
    /// working: every awake tab is one a sleep would take (a shell at its prompt, or an
    /// agent idle in a conversation it has named), and no agent waits on another.
    ///
    /// Refused when the binary this daemon started from does not say it is `version`.
    /// Otherwise the daemon waits, as long as it takes, and the awake tabs come back on the
    /// new binary with the same numbers, each agent resumed into its own conversation; a tab
    /// asleep stays asleep. Asking again while it waits is answered with what it is waiting
    /// on.
    ///
    /// `keep_tabs` and `quiet` are for a move nobody asked for by hand. `keep_tabs`: never end
    /// a tab, so the daemon moves only when it can hand every tab over live. `quiet`: move
    /// only with no client connected, since a live move cuts every client's connection. Asked
    /// again while it waits, a later ask can only weaken them. A daemon from before them
    /// ignores both, which is why a sender checks the version it runs first.
    ///
    /// `same_version`: a build that says the version this daemon runs is one to move onto too,
    /// which is what `qoop restart` asks after an install between two releases. A daemon from
    /// before it answers that it is already there and moves nothing.
    RestartWhenSafe {
        id: String,
        version: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep_tabs: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        quiet: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        same_version: bool,
    },
    /// Whether a newer qoop than this daemon runs is on disk: the version it could move onto,
    /// answered as `UpdateState`. Read only, and what the foot of the interface and the
    /// phone ask every half minute or so. A daemon from before it answers `unknown variant`.
    UpdateState {
        id: String,
    },
    /// This machine's sleep settings, read or changed: `[sleep] idle_minutes` and
    /// `[daemon] exit_when_asleep` in its `config.toml`, the file `qoop settings` edits. With
    /// neither field it is a read; with one or both it writes them in one rewrite of the file
    /// and answers what the file now says. Setting a value never sleeps or ends anything: the
    /// idle pass that follows vets every tab as it always does. A daemon from before it answers
    /// `unknown variant`.
    SleepSettings {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_minutes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_when_asleep: Option<bool>,
    },
    /// Whether qass watches the agents, read or set: with `on` it turns the owner's switch
    /// (qass's `watch` file) and answers what it now is. Turned on, the daemon types one line
    /// into qass's tab so it starts its watcher again. A daemon from before it answers
    /// `unknown variant`.
    QassWatch {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on: Option<bool>,
    },
    /// Until when a person is at a terminal (typing there, or a line just sent), so another
    /// sender does not type over them. A daemon from before it answers `unknown variant`.
    PersonAt {
        id: String,
        terminal: TerminalStream,
    },
    /// Write to a task: open one, move it, record its plan, evidence or done claim, or
    /// close it.
    ///
    /// `task` names one directly, which any client of this socket may do (the socket is the
    /// owner's own). Without it, the target is the open task of `terminal`, which must come
    /// with that terminal's token, checked like a hook report. `Close` always names its task.
    TaskWrite {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalStream>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<i64>,
        action: TaskOp,
    },
    /// Read tasks: one by `task` (or the open one of `terminal`, with its token) with its
    /// events newest first, older than `before` when given; with neither, every open task.
    /// A board item named by `task` comes with its card (`Tasks::item`). `board` asks for
    /// the board too (`Tasks::items`), which a daemon from before it never answers.
    TaskRead {
        id: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        board: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalStream>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<i64>,
    },
    /// Write to the owner's question inbox. `Add` and `Close` come from qass, with the token
    /// of a terminal in qass's own space; `Answer` and `Dismiss` are the owner's, from a
    /// client that speaks for no terminal, and are refused to any terminal's token.
    AskWrite {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalStream>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        action: AskOp,
    },
    /// The cards waiting for the owner and the last few that ended, answered as `Asks`.
    AskRead {
        id: String,
        /// Every card that ended in the last 14 days (up to 200), not the last five: what qass
        /// reconciles its notes against.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
    },
    /// Put a result on the owner's phone (`qoop show`): `target` is an absolute file path or
    /// an `http`/`https` url, sent with the token of the terminal whose agent shows it.
    /// Answered `ShowWritten`.
    ShowWrite {
        id: String,
        terminal: u32,
        token: String,
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// The newest results shown, answered as `Shows`.
    ShowRead {
        id: String,
    },
    /// The newest agent sessions on this computer a person typed into, whoever started them,
    /// answered as `AgentSessions`. `dirs` empty: every directory, else the ones in or under
    /// one of them; a range (epoch ms, `[from_ms, to_ms)`) keeps the sessions active in it.
    AgentSessions {
        id: String,
        #[serde(default)]
        dirs: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_ms: Option<i64>,
        limit: u32,
    },
    /// Tokens, prompts and sessions per day for the last `days` days, in a zone
    /// `tz_offset_min` minutes east of UTC, answered as `AgentActivity`.
    AgentActivity {
        id: String,
        days: u32,
        #[serde(default)]
        tz_offset_min: i32,
    },
    /// Up to `len` bytes (cut at [`SHOW_CHUNK`]) of a shown file from `offset`, answered as
    /// `ShowChunk`. Only the path recorded for that show is read.
    ShowFetch {
        id: String,
        show: u64,
        offset: u64,
        len: u32,
    },
    /// Open a byte tunnel to the Mac's own `localhost:<port>` of a shown loopback url,
    /// answered `TunnelOpened`. `conn` is the phone's number for the connection, rising and
    /// never reused. The bytes ride the Blob channel with `conn` as the stream, every frame
    /// END-flagged and at most [`BLOB_MOST`] bytes; an empty frame ends that side. The
    /// tunnel lives as long as the connection that asked.
    TunnelOpen {
        id: String,
        show: u64,
        conn: u32,
    },
    /// What `terminal`'s checkout changed against its default branch, answered `Changes`.
    Changes {
        id: String,
        terminal: u32,
    },
    /// Record a path or url the owner tapped in `terminal`'s chat (the phone's own): an
    /// absolute, `~` or relative path (under the terminal's folder) to a file the phone can
    /// view, or an `http`/`https` url. Answered `Shows` with that one show, which `ShowFetch`
    /// and `TunnelOpen` take by its id; it is not listed by `ShowRead`. `auto` is a load
    /// nobody tapped, refused outside the terminal's folder, `/tmp` and `$TMPDIR`.
    ShowOpen {
        id: String,
        terminal: u32,
        target: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        auto: bool,
    },
    /// Add, pause, resume, remove or run a schedule, or report how a run went (`qoop schedule`).
    /// A write from a terminal carries its token and is refused unless that terminal is qass's;
    /// a client that speaks for no terminal (the app, the CLI outside qoop, the run itself
    /// reporting) sends none. Answered `ScheduleWritten`.
    ScheduleWrite {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalStream>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        action: Box<ScheduleOp>,
    },
    /// Every schedule, answered as `Schedules`.
    ScheduleRead {
        id: String,
    },
}

/// What a `ScheduleWrite` does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleOp {
    /// Make a schedule: `cron` (five fields, local time) for a repeat or `at` (unix seconds)
    /// for one moment, exactly one of them. `agent` defaults to qass's agent, `space` is made
    /// when the run starts.
    Add {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cron: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<String>,
        prompt: String,
    },
    Pause {
        schedule: i64,
    },
    /// Fire again from now on, with nothing caught up for the time it was paused.
    Resume {
        schedule: i64,
    },
    Remove {
        schedule: i64,
    },
    /// Start it now, as a run of its own: its slot and its next time are not touched.
    Run {
        schedule: i64,
    },
    /// How the run of `slot` went (`started` with its terminal, or `failed: <why>`), from the
    /// run itself. Applied only while the schedule still holds that slot.
    Report {
        schedule: i64,
        slot: i64,
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<i64>,
    },
}

/// A schedule as a client lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleInfo {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    pub agent: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    pub prompt: String,
    pub paused: bool,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_slot: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<i64>,
    /// `claimed`, `started`, `skipped: <why>` or `failed: <why>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    /// The terminal of the last run, which may be closed by now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_terminal: Option<i64>,
    /// When it next comes due, in unix seconds; none when paused or done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<i64>,
    /// The daemon's words for the timing, the same on every client: `every day 09:00`,
    /// `weekdays 09:00`, `mondays 08:30`, `every 2h`, `every 15m`, `once oct 24 17:00`, else
    /// the cron text.
    pub when: String,
}

/// The most bytes of prompt a schedule keeps.
pub const SCHEDULE_PROMPT_MAX: usize = 8_000;

/// The most schedules there are at once.
pub const SCHEDULES_MOST: usize = 50;

/// The most bytes one Blob frame of a tunnel carries: inside one record of the phone's link.
pub const BLOB_MOST: usize = 60 * 1024;

/// One changed file of a `Changes` answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    /// git's letter against the base (`M`, `A`, `D`, `R`, `C`, `T`, `U`), or `?` untracked.
    pub status: String,
}

/// The pull request of a `Changes` answer's branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    /// `open`, `closed` or `merged`.
    pub state: String,
    /// `passing`, `failing`, `pending`, or `none` when the branch has no checks.
    pub checks: String,
}

/// The largest file `qoop show` puts on the phone.
pub const SHOW_MOST: u64 = 25 * 1024 * 1024;

/// The most bytes one `ShowFetch` answers: small enough to arrive well inside the
/// reassembly deadline on a slow link.
pub const SHOW_CHUNK: u32 = 128 * 1024;

/// An agent session as a history list shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    /// Unique per session, unlike `id`.
    pub key: String,
    /// `claude`, `codex`, `opencode` or `gemini`.
    pub agent: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Within the asked range, when there was one.
    pub tokens: u64,
    pub prompts: u32,
    /// The line that carries the conversation on, typed in `cwd`; none when the agent has no
    /// resume or the directory is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// The directory it ran in is no longer there.
    #[serde(default)]
    pub gone: bool,
}

/// A result as the phone lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowInfo {
    pub show: u64,
    /// `image`, `video`, `file` or `url`: an icon and a label. The viewer goes by the name.
    pub kind: String,
    /// The file's name (never its path), or the url itself.
    pub name: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Bytes when shown; 0 for a url.
    pub size: u64,
    /// When it was shown, in unix seconds.
    pub at: i64,
    pub terminal: u32,
    /// Whether the agent that showed it is still the one in that terminal: the same daemon,
    /// the terminal open, the same generation.
    pub current: bool,
    #[serde(default)]
    pub space: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// What an `AskWrite` does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskOp {
    /// Put a card in the inbox. `agent` is `T52`, `ubox/T2` or none, as `qoop ask add`
    /// normalized it.
    Add {
        card: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        body: Box<AskCard>,
    },
    /// The owner's answer: an option's key, his own words, or both.
    Answer {
        card: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// The words of `option` as the owner saw them. Refused when the card was rewritten
        /// since: he must never send words he did not read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seen: Option<String>,
    },
    /// End a card: it was answered in the chat, qass answered it, or its agent is done.
    Close { card: String, note: String },
    /// The owner puts a card away unanswered.
    Dismiss { card: String },
}

/// What an `AskWrite` came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskResult {
    Added,
    /// The same card again: nothing changed.
    Unchanged,
    /// An open card was rewritten.
    Replaced,
    /// His answer goes to the worker: typed at once, or when it is next at rest.
    Answered,
    /// His answer goes to qass, which relays it: typed at once, or when qass is at rest.
    Passed,
    /// What a daemon from before answers were queued said when qass could not be typed to:
    /// the card stayed open. Nothing produces it now; it stays so an older daemon's word parses.
    Busy,
    Closed,
    /// The card had already ended.
    Ended,
}

/// The longest one value of a card may be, in characters.
pub const ASK_FIELD_MAX: usize = 200;

/// One option of a card: what a tap sends and why it is offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskOption {
    /// `a` to `d`, in order.
    pub key: String,
    pub label: String,
    pub reason: String,
    /// What the worker is told when this option is picked.
    pub words: String,
}

/// The option qass would take, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskPick {
    pub key: String,
    pub reason: String,
}

/// A question card as qass writes it (`skills/qass/SKILL.md`). Identity (its number, the
/// agent, the urgency) is not in here: it is the arguments of `escalate.sh`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskCard {
    /// The word of the agent's sidebar name the owner types to answer: lowercase.
    pub handle: String,
    /// `now` rings his phone, `later` waits.
    pub urgency: String,
    /// What he asked the agent for. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
    pub doing: String,
    pub why: String,
    pub asks: String,
    /// Two to four, keyed `a` to `d` in order.
    pub options: Vec<AskOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<AskPick>,
}

impl AskCard {
    /// Every rule of the format that is not grammar: what is required, how long a value may
    /// be, which keys. All the problems at once, so the writer fixes them in one go.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        check_value(&mut out, "doing", &self.doing);
        check_value(&mut out, "why", &self.why);
        check_value(&mut out, "asks", &self.asks);
        if let Some(asked) = &self.asked {
            check_value(&mut out, "asked", asked);
        }
        for o in &self.options {
            let key = &o.key;
            check_value(&mut out, &format!("option {key} label"), &o.label);
            check_value(&mut out, &format!("option {key} reason"), &o.reason);
            check_value(&mut out, &format!("option {key} words"), &o.words);
        }
        if let Some(p) = &self.pick {
            check_value(&mut out, "pick reason", &p.reason);
        }
        let handle_ok = !self.handle.is_empty()
            && self.handle.chars().count() <= 40
            && self
                .handle
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !handle_ok {
            out.push("the handle is one lowercase word (a to z, 0 to 9, -), at most 40".into());
        }
        if !matches!(self.urgency.as_str(), "now" | "later") {
            out.push("the urgency is now or later".into());
        }
        let keys: Vec<&str> = self.options.iter().map(|o| o.key.as_str()).collect();
        if !(2..=4).contains(&keys.len()) || keys != ["a", "b", "c", "d"][..keys.len()] {
            out.push("two to four options, keyed A to D in order".into());
        }
        if let Some(p) = &self.pick
            && !keys.contains(&p.key.as_str())
        {
            out.push(format!("pick names {}, which is not an option", p.key));
        }
        out
    }
}

/// One value of a card: not empty, one line, within `ASK_FIELD_MAX`.
fn check_value(out: &mut Vec<String>, what: &str, text: &str) {
    if text.chars().any(char::is_control) {
        out.push(format!("{what} holds a control character or a newline"));
    }
    if text.trim().is_empty() {
        out.push(format!("{what} is empty"));
    }
    let len = text.chars().count();
    if len > ASK_FIELD_MAX {
        out.push(format!(
            "{what} is {len} characters, over the {ASK_FIELD_MAX} limit; shorten it"
        ));
    }
}

/// A card as the inbox shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskInfo {
    pub card: String,
    #[serde(default)]
    pub agent: Option<String>,
    /// The local terminal it is about, when it is about one.
    #[serde(default)]
    pub terminal: Option<u32>,
    /// `open`, `passed`, `answered` or `closed`.
    pub status: String,
    #[serde(default)]
    pub note: Option<String>,
    pub opened_at: i64,
    #[serde(default)]
    pub closed_at: Option<i64>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub option: Option<String>,
    #[serde(default)]
    pub task: Option<i64>,
    pub body: AskCard,
}

/// What a `TaskWrite` does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskOp {
    New {
        goal: String,
        replace: bool,
    },
    /// The terminal's open task, or a new one named after its newest prompt: what a hook
    /// sends at the agent's first file edit, so the record never waits on the agent.
    Begin,
    Stage {
        stage: String,
    },
    /// Rewrite the goal: what the owner wants, in plain words, with its done bar. From a
    /// terminal it lands on that terminal's live task, begun first as `Begin` does.
    Goal {
        goal: String,
    },
    Plan {
        text: String,
    },
    Evidence {
        text: String,
    },
    Done {
        summary: String,
    },
    Close {
        outcome: TaskOutcome,
        reason: String,
    },
    /// Put an item on a space's board: a task with no terminal, answered with its key. From
    /// anywhere, never in qass's own space.
    Add {
        workspace: i64,
        goal: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Describe, Steps and Check write on an item: by id, that item; from a terminal, the
    /// item its task is linked to, refused when it is not on the board.
    Describe {
        text: String,
    },
    /// Replace the item's steps; a step whose text is unchanged keeps its tick.
    Steps {
        steps: Vec<String>,
    },
    /// Tick step `n` (from 1), or untick it.
    Check {
        n: i64,
        done: bool,
    },
    /// Link a task to the item `item`. From a terminal, its live task, begun first as `Begin`
    /// does, and refused where a begin opens nothing (qass's space).
    Link {
        item: i64,
    },
    /// Take a task off its item: from a terminal, its open task; by id, that task, or an
    /// item, which loses every agent and a waiting start and so moves back to Todo.
    Unlink,
    /// Ask for the item named by id to be started: Doing for ten minutes, until an agent is
    /// linked. Answered with its key as stored.
    Start,
    /// Bring the closed item named by id back, its steps kept.
    Reopen,
    /// Put the closed item named by id away from the board, or (`undo`) bring it back on.
    /// Nothing of it is deleted: it stays closed, with its outcome and history.
    Dismiss {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        undo: bool,
    },
    /// qass reports an agent on an ssh remote working on `item`, from that machine's
    /// events: its task there, its name and its stage.
    LinkRemote {
        item: i64,
        remote: String,
        task: i64,
        name: String,
        stage: String,
    },
}

/// How a task is closed by a person or qass. Superseded happens to a task; nobody asks for
/// it. Dropped is abandoned, asked for: an item taken off the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Verified,
    Failed,
    Dropped,
}

/// A board item as the board shows it: no description and no step texts, so a whole board
/// fits one frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: i64,
    /// `<PREFIX>-<n>`, fixed when it was added.
    pub key: String,
    #[serde(default)]
    pub workspace: Option<i64>,
    pub title: String,
    /// `todo`, `doing` or `done`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub steps_done: usize,
    pub steps_total: usize,
    #[serde(default)]
    pub agents: Vec<ItemAgentInfo>,
    pub updated_at: i64,
    /// When it came into its lane (Done: when it closed), for "how long"; 0 from an older daemon.
    #[serde(default)]
    pub since: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// Put away by the owner (`dismiss`): closed, left out of the Done lane by every client.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// An agent working on an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemAgentInfo {
    pub task: i64,
    #[serde(default)]
    pub terminal: Option<i64>,
    pub name: String,
    /// `this Mac`, or the remote's name.
    pub machine: String,
    /// For a remote agent, the stage qass last reported.
    pub stage: String,
    #[serde(default)]
    pub claimed: bool,
    #[serde(default)]
    pub closed: bool,
    /// When it began on the item, for how long it has worked; 0 from an older daemon.
    #[serde(default)]
    pub since: i64,
}

/// One step of an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepInfo {
    pub n: i64,
    pub text: String,
    #[serde(default)]
    pub done_at: Option<i64>,
}

/// An item as its card shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCardInfo {
    #[serde(flatten)]
    pub item: ItemInfo,
    pub description: String,
    pub steps: Vec<StepInfo>,
    /// Its own events and its agents', newest first, each with the task it is on.
    pub activity: Vec<ItemEventInfo>,
}

/// One line of an item's activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemEventInfo {
    pub task: i64,
    pub at: i64,
    pub kind: String,
    pub body: String,
}

/// What a machine's own agents are called on the board.
pub const THIS_MACHINE: &str = "this Mac";

/// What every reader calls a local agent, in one order: its agent title (what a person called
/// the conversation here, else the agent's own), the conversation's stored name, the tab's
/// name, the head of the conversation's id, its terminal. The same everywhere, so the board,
/// its Markdown and the sidebar never name one agent two ways.
pub fn agent_name(
    terminal: i64,
    title: Option<String>,
    stored: Option<String>,
    tab: Option<String>,
    session: Option<&str>,
) -> String {
    title
        .or(stored)
        .or(tab)
        .or_else(|| session.map(|s| s.chars().take(8).collect()))
        .unwrap_or_else(|| format!("terminal {terminal}"))
}

/// What a local agent's stage reads once its task is closed.
pub const FINISHED: &str = "finished";

/// A stored item as the protocol carries it. `name` names a local agent by its terminal
/// ([`agent_name`]), for a terminal still open; a closed one keeps the name it was linked
/// under (its terminal may be gone, or its number handed to another tab), and its stage
/// reads [`FINISHED`]. A remote one comes with the name and stage qass reported.
pub fn item_info(row: qoop_store::ItemRow, name: impl Fn(i64) -> Option<String>) -> ItemInfo {
    let agents = row
        .agents
        .into_iter()
        .map(|a| {
            let live = a.terminal.filter(|_| !a.closed).and_then(&name);
            let local = a.remote.is_none();
            ItemAgentInfo {
                // A closed agent never borrows the name of whatever tab holds its old number now.
                name: live
                    .or(a.name)
                    .or_else(|| a.terminal.filter(|_| !a.closed).and_then(&name))
                    .unwrap_or_else(|| match a.terminal {
                        Some(t) => format!("terminal {t}"),
                        None => format!("task #{}", a.task),
                    }),
                machine: a.remote.unwrap_or_else(|| THIS_MACHINE.to_string()),
                task: a.task,
                terminal: a.terminal,
                stage: if a.closed && local {
                    FINISHED.to_string()
                } else {
                    a.stage
                },
                claimed: a.claimed,
                closed: a.closed,
                since: a.since,
            }
        })
        .collect();
    ItemInfo {
        id: row.task.id,
        key: row.key,
        workspace: row.task.workspace,
        title: row.task.goal,
        state: row.state.as_str().to_string(),
        outcome: row.task.outcome,
        steps_done: row.steps_done,
        steps_total: row.steps_total,
        agents,
        updated_at: row.updated_at,
        since: row.since,
        closed_at: row.task.closed_at,
        hidden: row.hidden,
    }
}

/// A task as a row in the agents listing: enough to say what the terminal is working on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub id: i64,
    /// Cut to `TASK_REF_GOAL` characters: every row carries it, and the listing is one frame.
    pub goal: String,
    pub stage: String,
    /// The agent said it is done and nothing has moved it since.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claimed: bool,
}

/// How much of a goal an agents row carries.
pub const TASK_REF_GOAL: usize = 100;

/// A task as `qoop task` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: i64,
    #[serde(default)]
    pub workspace: Option<i64>,
    #[serde(default)]
    pub terminal: Option<i64>,
    pub goal: String,
    pub stage: String,
    pub opened_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub claimed_at: Option<i64>,
    #[serde(default)]
    pub closed_at: Option<i64>,
    #[serde(default)]
    pub outcome: Option<String>,
    /// Where its agent works, kept past its tab (see `TaskRow::folder`).
    #[serde(default)]
    pub folder: Option<String>,
}

/// One thing recorded on a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventInfo {
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub body: String,
}

impl ControlRequest {
    /// `SaveUpload` of `bytes`, encoded the way the daemon reads them, for `terminal`.
    pub fn save_upload(id: String, ext: &str, bytes: &[u8], terminal: u32) -> Self {
        use base64::Engine as _;
        Self::SaveUpload {
            id,
            ext: ext.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            terminal: Some(terminal),
        }
    }

    pub fn request_id(&self) -> &str {
        match self {
            Self::CreateTerminal { id, .. }
            | Self::RenameTerminal { id, .. }
            | Self::RenameSession { id, .. }
            | Self::MoveTerminal { id, .. }
            | Self::RenameWorkspace { id, .. }
            | Self::SetWorkspaceInk { id, .. }
            | Self::SetSpaceGoal { id, .. }
            | Self::MoveWorkspace { id, .. }
            | Self::SetKeepAwake { id, .. }
            | Self::SetQassSkip { id, .. }
            | Self::SetParked { id, .. }
            | Self::Shutdown { id, .. }
            | Self::Scrollback { id, .. }
            | Self::SearchScrollback { id, .. }
            | Self::Messages { id, .. }
            | Self::MessageDetail { id, .. }
            | Self::CloseTerminal { id, .. }
            | Self::ListTerminals { id }
            | Self::ListAgents { id }
            | Self::Focus { id, .. }
            | Self::News { id, .. }
            | Self::ListWorkspaces { id }
            | Self::CreateWorkspace { id, .. }
            | Self::CloseWorkspace { id, .. }
            | Self::SleepWorkspace { id, .. }
            | Self::WakeWorkspace { id, .. }
            | Self::Subscribe { id, .. }
            | Self::Unsubscribe { id, .. }
            | Self::RequestOwnership { id, .. }
            | Self::ReportAgentState { id, .. }
            | Self::ReportAgentSession { id, .. }
            | Self::ReportSubagents { id, .. }
            | Self::ListFocusEvents { id, .. }
            | Self::HostColors { id, .. }
            | Self::SaveUpload { id, .. }
            | Self::Listen { id, .. }
            | Self::SetSplit { id, .. }
            | Self::RestartWhenSafe { id, .. }
            | Self::UpdateState { id }
            | Self::SleepSettings { id, .. }
            | Self::QassWatch { id, .. }
            | Self::PersonAt { id, .. }
            | Self::TaskWrite { id, .. }
            | Self::TaskRead { id, .. }
            | Self::AskWrite { id, .. }
            | Self::AskRead { id, .. }
            | Self::ShowWrite { id, .. }
            | Self::ShowRead { id }
            | Self::AgentSessions { id, .. }
            | Self::AgentActivity { id, .. }
            | Self::ShowFetch { id, .. }
            | Self::TunnelOpen { id, .. }
            | Self::Changes { id, .. }
            | Self::ShowOpen { id, .. }
            | Self::ScheduleWrite { id, .. }
            | Self::ScheduleRead { id }
            | Self::ReloadAgentManifests { id }
            | Self::ConnectRemote { id, .. }
            | Self::InstallRemote { id, .. }
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
    /// The interfaces told to show a terminal, by the pid of each one's process, so the
    /// sender can raise the window the interface runs in. Empty when nobody is attached.
    Focused {
        id: String,
        clients: Vec<u32>,
    },
    Agents {
        id: String,
        agents: Vec<AgentRow>,
        /// The stored split trees, each cut down to the terminals alive now and sent
        /// only while two or more are left. In the same message as the rows so the two
        /// never disagree. Defaulted, so an older daemon reads as "no splits".
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        splits: Vec<Pane>,
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
    /// Tabs of a workspace are asleep: `terminals` terminals ended with their rows kept.
    WorkspaceAsleep {
        id: String,
        workspace: i64,
        terminals: usize,
        /// Which terminals went to sleep. Always present from a daemon that sleeps one tab
        /// at a time, empty when nothing needed to; absent (`None`) only from an older
        /// daemon, which put the whole workspace to sleep whatever was asked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed: Option<Vec<TerminalStream>>,
        /// The tabs that stayed awake, and why.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stayed: Vec<Stayed>,
    },
    /// Tabs of a workspace are awake again: `terminals` reopened, `resumed` of them agents
    /// typed back into their own conversations.
    WorkspaceAwake {
        id: String,
        workspace: i64,
        terminals: usize,
        resumed: usize,
        /// Which terminals came back, as `WorkspaceAsleep::changed` says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed: Option<Vec<TerminalStream>>,
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
    /// A page of a terminal's chat, newest items last. `revision` stamps the session file at
    /// the read; `reset` says the client's revision was not it, so `skip` was ignored and
    /// this is the newest page. `first_seq` is the `seq` of the first (oldest) item in this
    /// response: its distance from the newest item (an order, not a cursor), `has_older` that more lie before it, and
    /// `warnings` how many records the parser could not use. At most 256 KiB of items, and
    /// never a raw record.
    Messages {
        id: String,
        revision: u64,
        reset: bool,
        first_seq: u32,
        has_older: bool,
        items: Vec<qoop_parsed::Item>,
        warnings: u32,
    },
    /// What `MessageDetail` asked for: `truncated` when `input` or `output` hit the limit.
    MessageDetail {
        id: String,
        input: String,
        output: String,
        truncated: bool,
    },
    /// Where a `SearchScrollback` step got to.
    ///
    /// `row` is the first match in the direction asked, `None` when this step found none.
    /// `resume` is where the next step should start when the step ran out of budget before
    /// the end of the history, and `None` when the history was searched to its end.
    ///
    /// `col` is the cell the match starts in: one cell per character of the stored row, so
    /// the daemon never reasons about how wide anything is drawn. The client turns it into
    /// a screen column with its own width module.
    ScrollbackMatch {
        id: String,
        terminal: TerminalStream,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row: Option<u64>,
        #[serde(default)]
        col: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<u64>,
    },
    /// The daemon heard, and is going. The count is what it is ending.
    Stopping {
        id: String,
        terminals: usize,
    },
    Renamed {
        id: String,
    },
    /// A terminal is in `workspace` now.
    TerminalMoved {
        id: String,
        terminal: TerminalStream,
        workspace: i64,
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
    /// A `SetSplit` is on disk.
    SplitSaved {
        id: String,
    },
    /// A `RestartWhenSafe` was taken: the daemon moves once nothing is working. Not sent
    /// when the first look found nothing in the way, because then the daemon is already
    /// going and its connections close.
    RestartArmed {
        id: String,
        /// What the last look found still working, as a person would be told it ("tab 3:
        /// sleep is running in it"). `None` when no look has finished yet, and when this
        /// daemon already runs that version or a newer one, so there is nothing to move to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waiting_on: Option<String>,
    },
    /// What an `UpdateState` found: the version the qoop on disk says it is when that is newer
    /// than the one this daemon runs (which is what a click on `restart for update` asks to
    /// move onto), and nothing when there is none or this daemon cannot move.
    UpdateState {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ready: Option<String>,
    },
    /// What a `SleepSettings` found in the file: the minutes a tab idles before it sleeps by
    /// itself (`0` is never) and whether the daemon leaves once nothing is awake. `problem`
    /// says why a value in the file could not be used (it then shows its default); a file that
    /// is not TOML is a `Failed`, not defaults.
    SleepSettings {
        id: String,
        idle_minutes: u64,
        exit_when_asleep: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// What a `QassWatch` left the switch at.
    QassWatch {
        id: String,
        on: bool,
    },
    /// The answer to `PersonAt`: until when, as unix seconds, `None` when nobody is there.
    PersonAt {
        id: String,
        until: Option<i64>,
    },
    /// A `TaskWrite` landed on `task`; `key` is an item's, when one was added or started.
    TaskWritten {
        id: String,
        task: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// What a `TaskRead` asked for, kept under one frame: `more` says the list or the events
    /// were cut there, and older events come with `before`. `space` is the asking terminal's
    /// space name, when one was asked about and it could be read in time.
    Tasks {
        id: String,
        tasks: Vec<TaskInfo>,
        #[serde(default)]
        events: Vec<TaskEventInfo>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        more: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<String>,
        /// The board, when it was asked for. `None` from a daemon from before the board,
        /// which a client shows as too old rather than as an empty board.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        items: Option<Vec<ItemInfo>>,
        /// The card of the item `task` named.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<Box<ItemCardInfo>>,
    },
    /// An `AskWrite` landed on `card`.
    AskWritten {
        id: String,
        card: String,
        result: AskResult,
    },
    /// The inbox: cards waiting (oldest first) and the last few that ended (newest first).
    Asks {
        id: String,
        live: Vec<AskInfo>,
        ended: Vec<AskInfo>,
    },
    /// A `ShowWrite` was recorded as `show`.
    ShowWritten {
        id: String,
        show: u64,
    },
    /// `reading`: the sessions are still being read for the first time; ask again.
    AgentSessions {
        id: String,
        reading: bool,
        sessions: Vec<SessionRow>,
    },
    /// Only days with something in them, oldest first.
    AgentActivity {
        id: String,
        reading: bool,
        days: Vec<qoop_parsed::index::Day>,
    },
    /// The newest results shown, newest first, at most 50.
    Shows {
        id: String,
        shows: Vec<ShowInfo>,
    },
    /// A `ScheduleWrite` was applied to `schedule`.
    ScheduleWritten {
        id: String,
        schedule: i64,
    },
    /// Every schedule, oldest first.
    Schedules {
        id: String,
        schedules: Vec<ScheduleInfo>,
    },
    /// Bytes of a shown file, as standard base64, with the file's size and modification
    /// time (unix milliseconds) now: a reader starts over when either changed.
    ShowChunk {
        id: String,
        data: String,
        total: u64,
        mtime: u64,
    },
    /// `TunnelOpen` reached the port: Blob frames on stream `conn` carry its bytes.
    TunnelOpened {
        id: String,
        conn: u32,
    },
    /// A terminal's checkout against `base`: `diff` is `git diff` from where the two met,
    /// committed and uncommitted together (untracked files are only listed), and `cut` when
    /// it or the list was longer than 256 KiB. `branch` is `None` on a detached HEAD. `pr` is
    /// the branch's newest pull request; `pr_known` says gh answered, so a `None` there means
    /// there is none rather than that nobody could ask.
    Changes {
        id: String,
        root: String,
        branch: Option<String>,
        base: String,
        files: Vec<ChangedFile>,
        diff: String,
        cut: bool,
        pr: Option<PullRequest>,
        pr_known: bool,
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
    /// The machine is added under `name`, and its spaces are on their way.
    RemoteAdded {
        id: String,
        name: String,
    },
    /// The machine has no qoop, and is one qoop is built for: `platform` as shown to a
    /// person ("linux x86_64"), and whether the connection logs in as root.
    RemoteMissing {
        id: String,
        target: String,
        platform: String,
        root: bool,
    },
    /// qoop `version` is now on the machine.
    RemoteInstalled {
        id: String,
        target: String,
        version: String,
    },
    /// `HostColors` was taken: programs asking from now on get these.
    HostColorsNoted {
        id: String,
    },
    /// `SaveUpload` kept the file here.
    Uploaded {
        id: String,
        path: String,
    },
    /// `Listen` may go on: a probe is answered, a recording has started.
    Listening {
        id: String,
    },
    /// What the lent microphone heard for `Listen`: 16 kHz mono signed 16-bit little
    /// endian samples, as standard base64.
    Heard {
        id: String,
        data: String,
    },
    /// `News` was taken: the rows say it from now on.
    NewsNoted {
        id: String,
    },
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

/// One tab's split tree, as it crosses the socket.
///
/// The client owns the tree and the daemon only keeps it, so this is the client's shape
/// with nothing the daemon would have to understand. Typed rather than a string so the
/// ssh mux can renumber the terminals in it. The ratio is in thousandths, which keeps the
/// type `Eq` like everything else on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Pane {
    Leaf {
        terminal: TerminalStream,
    },
    Split {
        dir: SplitDir,
        permille: u16,
        /// qass's questions column, in columns: the width the owner dragged it to. Zero is not
        /// set (the default width), and is not written, so an older reader sees the tree it knew.
        #[serde(default, skip_serializing_if = "is_zero")]
        questions: u32,
        first: Box<Pane>,
        second: Box<Pane>,
    },
}

/// `V` puts the panes side by side, `H` stacks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDir {
    V,
    H,
}

impl Pane {
    /// The terminals in the tree, in tree order. The first one is the tab.
    pub fn leaves(&self) -> Vec<TerminalStream> {
        match self {
            Self::Leaf { terminal } => vec![*terminal],
            Self::Split { first, second, .. } => {
                let mut out = first.leaves();
                out.extend(second.leaves());
                out
            }
        }
    }

    /// The tree with only the terminals `keep` accepts. A split that lost one side
    /// becomes the other side; `None` when nothing is left.
    pub fn retain(&self, keep: &impl Fn(TerminalStream) -> bool) -> Option<Pane> {
        match self {
            Self::Leaf { terminal } => keep(*terminal).then(|| self.clone()),
            Self::Split {
                dir,
                permille,
                questions,
                first,
                second,
            } => match (first.retain(keep), second.retain(keep)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    dir: *dir,
                    permille: *permille,
                    questions: *questions,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (one, other) => one.or(other),
            },
        }
    }

    /// Every terminal renumbered, or `None` when any one cannot be.
    pub fn map(&self, f: &impl Fn(TerminalStream) -> Option<TerminalStream>) -> Option<Pane> {
        match self {
            Self::Leaf { terminal } => Some(Self::Leaf {
                terminal: f(*terminal)?,
            }),
            Self::Split {
                dir,
                permille,
                questions,
                first,
                second,
            } => Some(Self::Split {
                dir: *dir,
                permille: *permille,
                questions: *questions,
                first: Box::new(first.map(f)?),
                second: Box::new(second.map(f)?),
            }),
        }
    }
}

/// One workspace, as a client lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: i64,
    pub name: String,
    /// How many terminals are open in it, so a client can show a workspace as busy or
    /// empty without asking about every terminal in it.
    pub terminals: usize,
    /// Asleep: it has tabs and every one of them sleeps, so nothing in it runs. Defaulted
    /// so an older daemon's answer reads as awake.
    #[serde(default)]
    pub asleep: bool,
    /// Its sleeping tabs, whether or not anything else in it is awake.
    #[serde(default)]
    pub sleeping: Vec<SleepingTab>,
    /// Whether this daemon will close this space when asked. False for the session's
    /// default, which is where a client that names no workspace lands and which cannot be
    /// dropped without every later default creation becoming a foreign-key refusal.
    ///
    /// Defaulted false, not true: an older daemon omits the field, and a client reading
    /// that as "yes" offers a close that is always refused. Missing means "no", and the
    /// cost of missing is one control that is not drawn.
    #[serde(default)]
    pub closable: bool,
    /// The colour a person picked for it, one of the six space inks. Absent is the colour
    /// its id gives it, which is also what an older daemon's row reads as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<u8>,
    /// How the machine a space lives on is reached, for a space of another machine. Set
    /// only by the interface that merges the machines' lists; a daemon never sends it, so
    /// a space of this machine has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
    /// A person asked to keep it awake: idle sleep passes its tabs by. Written only when
    /// true, and false from a daemon that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_awake: bool,
    /// A person asked qass to leave it alone: the space's own flag, which every tab in it
    /// takes. Written only when true, and false from a daemon that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub qass_skip: bool,
    /// What the owner wants from the work in it, one line, when he said. Absent from a
    /// daemon that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
}

/// Whether the machine behind a space answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// Connected: what the row says is the machine's own answer.
    Up,
    /// Let go while nothing ran there: not connected, and not broken either.
    Parked,
    /// Connecting, or not answering.
    Down,
}

/// A tab that stayed awake when its workspace was put to sleep, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stayed {
    pub terminal: TerminalStream,
    pub reason: String,
}

/// A sleeping terminal, as its row in the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepingTab {
    /// The number it comes back under.
    pub terminal: u32,
    /// The tab it is a pane of, named by its first pane: the panes of one split share it,
    /// and a list shows one row per tab. Defaulted to the terminal itself for an older
    /// daemon, where every sleeping terminal is its own row.
    #[serde(default)]
    pub lead: Option<u32>,
    /// What a person called it, if anyone did.
    pub title: Option<String>,
    /// The agent that was in it, resumed into its own conversation on wake.
    pub agent: Option<String>,
    /// What that agent called the conversation, read from its own storage when listed:
    /// the name an awake row shows, so a sleep does not turn it back into a bare agent.
    #[serde(default)]
    pub session: Option<String>,
    /// Where it was, and where it starts again.
    pub cwd: String,
}

impl SleepingTab {
    /// What a list calls this tab, in the order an awake row is named: the conversation's
    /// name (what a person called it here, else the agent's own), else the tab's, else the
    /// agent, else a shell. A tab keeps the name it had awake when it falls asleep.
    pub fn label(&self) -> &str {
        [
            self.session.as_deref().map(str::trim),
            self.title.as_deref().map(str::trim),
            self.agent.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
        .unwrap_or("shell")
    }
}

/// What a terminal threw away rather than drew, as an index row carries it.
///
/// Its own shape and not the engine's, for the reason every type here is: a counter
/// renamed in the engine must not change what crosses the socket. Counters only, so a
/// field added later reads as zero from an older daemon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Discards {
    /// Control strings over their bound, dropped whole, and their bytes.
    pub control_strings: u64,
    pub control_string_bytes: u64,
    /// Image envelopes over their bound or deadline, dropped whole, and their bytes.
    pub images: u64,
    pub image_bytes: u64,
    /// Feeds that found the scrollback ring full, and so may have lost rows unseen.
    pub evicted_unobserved: u64,
    /// Pictures the daemon's flood guard dropped before any client was sent them.
    pub images_flooded: u64,
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
    /// What `qoop agents explain` shows as the authority behind the state. Absent
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
    /// What the agent is doing right now, as its own hook last said it: the newest tool
    /// label (`Edit src/a.rs`, `Bash cargo test`), or `thinking` after a prompt and before
    /// any tool. Only while the agent is working, starting, blocked or failed, and `None`
    /// otherwise or from a daemon that predates it. Raw: the client words it. The prompt
    /// itself never travels here: focus events stay out of the push payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// The terminal's foreground process group on the daemon's machine, for a client on the
    /// same machine that shows what the agent costs. Meaningless anywhere else: a client
    /// never reads it for a terminal of another machine. `None` from an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<i32>,
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
    /// What this terminal's engine threw away rather than parsed or kept, counted since
    /// the terminal started: control strings over their bound, image envelopes over theirs,
    /// feeds that may have lost scrollback unseen. Present only once something was, which
    /// is what lets `qoop agents explain` say a screen is missing something rather than
    /// leave the discard silent. Absent on rows from a daemon that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discards: Option<Discards>,
    /// Whether an interface counts this terminal as news for the person: an agent that
    /// finished while they were on another tab, or a `done` they have not read. The
    /// interface decides, since it is what showed the person the terminal; it says so with
    /// `News`, and this is true while any attached interface holds it. What the notch
    /// shows as done. Written only when true, and false from a daemon that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub news: bool,
    /// A person asked to keep this pane awake. Only the pane's own flag: its space's is on
    /// the space's row. Written only when true, and false from a daemon that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_awake: bool,
    /// qass leaves this agent alone: its pane's own flag or its space's, so a reader needs
    /// no second look. A client that toggles it per tab reads the space's row first. Written
    /// only when true, and false from a daemon that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub qass_skip: bool,
    /// A person parked this agent to answer later. Clients show it apart only while the agent
    /// is idle or done: a blocked or failed one shows as such. Written only when true, and
    /// false from a daemon that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parked: bool,
    /// How many things the agent has running in the background that its own state does not
    /// show: shells and monitors its footer counts, and subagents. An agent with its turn over
    /// reads `idle` while these run, so a client says so beside it. The state is left alone on
    /// purpose: scripts that wait on `idle` keep their meaning. Written only when above zero,
    /// and zero from a daemon that predates it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub background: u32,
    /// How many sub-agents the agent runs now, in its turn or in the background: what its
    /// hooks said started and has not ended, or what its screen lists, whichever is more.
    /// Also counted in `background`, so `background - subagents` is its shells and monitors.
    /// Written only when above zero, and zero from a daemon that predates it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subagents: u32,
    /// The task the agent in this terminal opened and has not had closed (`qoop task`).
    /// Absent when there is none, and from a daemon that predates tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRef>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
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

/// The first daemon version that keeps what `RestartWhenSafe`'s `keep_tabs` and `quiet` promise.
/// An older one ignores both fields and moves the old way (tabs ended, windows closed), so no
/// move nobody asked for by hand is ever sent to it.
pub const KEEPS_PROMISES_FROM: &str = "0.9.2";

/// What a daemon that moves onto a build of its own version (`RestartWhenSafe`'s
/// `same_version`) says among the welcome's capabilities. One without it stays where it is.
pub const SAME_VERSION_RESTART: &str = "restart-same-version";

/// The welcome's key for the tabs a daemon took over running when it became its binary.
pub const ADOPTED: &str = "adopted";

/// The welcome's key for a mark of the daemon's process image, new at every exec.
pub const BOOT: &str = "boot";

/// Whether a daemon running `version` keeps those promises.
pub fn keeps_promises(version: &str) -> bool {
    crate::paths::parse_version(version)
        .is_some_and(|v| Some(v) >= crate::paths::parse_version(KEEPS_PROMISES_FROM))
}

/// Whether a refusal is a daemon older than the request: it says `unknown variant`, and when
/// the request is a single one that `variant` names, that it is that one. A daemon that could
/// not read the request's id answers with none, so callers match on this and not on the id.
pub fn too_old_for(reason: &str, variant: Option<&str>) -> bool {
    reason.contains("unknown variant")
        && variant.is_none_or(|variant| reason.contains(&format!("`{variant}`")))
}

impl ControlResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::TerminalCreated { id, .. }
            | Self::TerminalClosed { id, .. }
            | Self::Terminals { id, .. }
            | Self::Agents { id, .. }
            | Self::Focused { id, .. }
            | Self::Workspaces { id, .. }
            | Self::WorkspaceCreated { id, .. }
            | Self::WorkspaceClosed { id, .. }
            | Self::WorkspaceAsleep { id, .. }
            | Self::WorkspaceAwake { id, .. }
            | Self::Renamed { id, .. }
            | Self::TerminalMoved { id, .. }
            | Self::Stopping { id, .. }
            | Self::Scrollback { id, .. }
            | Self::ScrollbackMatch { id, .. }
            | Self::Messages { id, .. }
            | Self::MessageDetail { id, .. }
            | Self::Subscribed { id, .. }
            | Self::OwnershipRequested { id, .. }
            | Self::OwnershipGranted { id, .. }
            | Self::AgentStateReported { id, .. }
            | Self::ManifestsReloaded { id, .. }
            | Self::FocusEvents { id, .. }
            | Self::RemoteAdded { id, .. }
            | Self::RemoteMissing { id, .. }
            | Self::RemoteInstalled { id, .. }
            | Self::HostColorsNoted { id }
            | Self::Uploaded { id, .. }
            | Self::Listening { id }
            | Self::Heard { id, .. }
            | Self::NewsNoted { id }
            | Self::SplitSaved { id }
            | Self::RestartArmed { id, .. }
            | Self::UpdateState { id, .. }
            | Self::SleepSettings { id, .. }
            | Self::QassWatch { id, .. }
            | Self::PersonAt { id, .. }
            | Self::TaskWritten { id, .. }
            | Self::Tasks { id, .. }
            | Self::AskWritten { id, .. }
            | Self::Asks { id, .. }
            | Self::ShowWritten { id, .. }
            | Self::Shows { id, .. }
            | Self::AgentSessions { id, .. }
            | Self::AgentActivity { id, .. }
            | Self::ShowChunk { id, .. }
            | Self::ScheduleWritten { id, .. }
            | Self::Schedules { id, .. }
            | Self::TunnelOpened { id, .. }
            | Self::Changes { id, .. }
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

/// What a hook says about an agent's sub-agents, each named by the agent's own id for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentChange {
    Started(String),
    Ended(String),
    /// Every sub-agent that runs now, from the agent's own list: one held and missing from it
    /// has ended. Never adds one: a start comes as `Started`, and a list read before an end
    /// that lands first would bring that one back.
    Running(Vec<String>),
}

/// Cut a focus label to [`MAX_FOCUS_LABEL_CHARS`] characters.
///
/// Runs of whitespace collapse to one space first: a label travels on one
/// row, and a multi-line prompt head would otherwise break the row it is
/// drawn on.
/// The secrets a command line tends to carry are masked first, by `qoop_parsed::redact`,
/// where every label enters.
pub fn truncate_focus_label(label: &str) -> String {
    // Control characters out first: a label is shown on screens and printed by
    // `qoop agents --json`, and an escape in a command must not travel as one.
    let clean: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let single = redact(&clean.split_whitespace().collect::<Vec<_>>().join(" "));
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
pub fn parse_reported_state(name: &str) -> Option<qoop_domain::AgentRuntimeState> {
    use qoop_domain::AgentRuntimeState as S;
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

/// The id of a request that did not parse, when its JSON still names one.
///
/// A request from a newer client carries an op this daemon has never heard of. Answering
/// its refusal with an empty id leaves the client unable to tell whose refusal it is, so
/// the id is read out of the raw object when there is one.
pub fn raw_request_id(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("id")?.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One chain names a local agent in every reader: the agent title, the stored name, the
    /// tab's name, the head of the conversation id, then its terminal.
    #[test]
    fn an_agent_is_named_by_one_chain() {
        let some = |s: &str| Some(s.to_string());
        let id = Some("0123456789abcdef");
        assert_eq!(
            agent_name(3, some("title"), some("stored"), some("tab"), id),
            "title"
        );
        assert_eq!(
            agent_name(3, None, some("stored"), some("tab"), id),
            "stored"
        );
        assert_eq!(agent_name(3, None, None, some("tab"), id), "tab");
        assert_eq!(agent_name(3, None, None, None, id), "01234567");
        assert_eq!(agent_name(3, None, None, None, None), "terminal 3");
    }

    fn leaf(terminal: u32) -> Pane {
        Pane::Leaf { terminal }
    }

    fn split(dir: SplitDir, first: Pane, second: Pane) -> Pane {
        Pane::Split {
            dir,
            permille: 500,
            questions: 0,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Two by two: 1 and 2 side by side over 3 and 4.
    fn quad() -> Pane {
        split(
            SplitDir::H,
            split(SplitDir::V, leaf(1), leaf(2)),
            split(SplitDir::V, leaf(3), leaf(4)),
        )
    }

    #[test]
    fn a_split_tree_keeps_its_shape_across_the_wire() {
        let json = serde_json::to_string(&quad()).expect("encode");
        let back: Pane = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, quad());
        assert_eq!(back.leaves(), vec![1, 2, 3, 4]);
    }

    /// The width of qass's questions column rides on the root split: it is written only when set,
    /// read as unset from a tree without it, and kept by the two functions that rebuild a tree.
    #[test]
    fn the_questions_width_is_kept_by_the_wire_retain_and_map() {
        let Pane::Split {
            dir,
            permille,
            first,
            second,
            ..
        } = quad()
        else {
            unreachable!("quad is a split")
        };
        let wide = Pane::Split {
            dir,
            permille,
            questions: 70,
            first,
            second,
        };
        let json = serde_json::to_string(&wide).expect("encode");
        assert!(json.contains("\"questions\":70"), "{json}");
        assert_eq!(serde_json::from_str::<Pane>(&json).expect("decode"), wide);
        assert_eq!(wide.retain(&|_| true), Some(wide.clone()));
        assert_eq!(wide.map(&Some), Some(wide.clone()));
        let plain = serde_json::to_string(&quad()).expect("encode");
        assert!(!plain.contains("questions"), "{plain}");
        let old = r#"{"k":"split","dir":"v","permille":500,"first":{"k":"leaf","terminal":1},"second":{"k":"leaf","terminal":2}}"#;
        assert_eq!(
            serde_json::from_str::<Pane>(old).expect("decode"),
            split(SplitDir::V, leaf(1), leaf(2))
        );
    }

    #[test]
    fn retain_collapses_a_split_that_lost_a_side() {
        let kept = quad().retain(&|t| t != 2 && t != 3).expect("two left");
        assert_eq!(kept, split(SplitDir::H, leaf(1), leaf(4)));
        assert_eq!(quad().retain(&|t| t == 4), Some(leaf(4)));
        assert_eq!(quad().retain(&|_| false), None);
    }

    #[test]
    fn map_renumbers_every_leaf_or_gives_up() {
        let moved = quad().map(&|t| Some(t + 10)).expect("all map");
        assert_eq!(moved.leaves(), vec![11, 12, 13, 14]);
        assert_eq!(quad().map(&|t| (t != 3).then_some(t)), None);
    }

    #[test]
    fn an_index_from_an_older_daemon_has_no_splits() {
        let answer: ControlResponse =
            serde_json::from_str(r#"{"op":"Agents","id":"i","agents":[]}"#).expect("decode");
        assert_eq!(
            answer,
            ControlResponse::Agents {
                id: "i".into(),
                agents: Vec::new(),
                splits: Vec::new(),
            }
        );
        // And an index with none says nothing about them, for an older client.
        let json = serde_json::to_string(&answer).expect("encode");
        assert!(!json.contains("splits"), "{json}");
    }

    #[test]
    fn sleep_settings_cross_the_wire_and_a_bare_one_is_a_read() {
        let read = ControlRequest::SleepSettings {
            id: "s".into(),
            idle_minutes: None,
            exit_when_asleep: None,
        };
        let json = serde_json::to_string(&read).expect("encode");
        assert_eq!(json, r#"{"op":"SleepSettings","id":"s"}"#);
        assert_eq!(
            serde_json::from_str::<ControlRequest>(&json).expect("decode"),
            read
        );

        let write = ControlRequest::SleepSettings {
            id: "w".into(),
            idle_minutes: Some(15),
            exit_when_asleep: Some(false),
        };
        let json = serde_json::to_string(&write).expect("encode");
        assert_eq!(
            serde_json::from_str::<ControlRequest>(&json).expect("decode"),
            write
        );
        assert_eq!(write.request_id(), "w");

        let answer = ControlResponse::SleepSettings {
            id: "w".into(),
            idle_minutes: 15,
            exit_when_asleep: false,
            problem: None,
        };
        let json = serde_json::to_string(&answer).expect("encode");
        assert!(!json.contains("problem"), "{json}");
        assert_eq!(
            serde_json::from_str::<ControlResponse>(&json).expect("decode"),
            answer
        );
        assert_eq!(answer.request_id(), "w");
    }

    #[test]
    fn qass_watch_crosses_the_wire_and_a_bare_one_is_a_read() {
        let read = ControlRequest::QassWatch {
            id: "q".into(),
            on: None,
        };
        let json = serde_json::to_string(&read).expect("encode");
        assert_eq!(json, r#"{"op":"QassWatch","id":"q"}"#);
        assert_eq!(
            serde_json::from_str::<ControlRequest>(&json).expect("decode"),
            read
        );
        let answer = ControlResponse::QassWatch {
            id: "q".into(),
            on: false,
        };
        let json = serde_json::to_string(&answer).expect("encode");
        assert_eq!(json, r#"{"op":"QassWatch","id":"q","on":false}"#);
        assert_eq!(answer.request_id(), "q");
    }

    #[test]
    fn a_daemon_without_an_op_refuses_it_in_words_a_client_can_test_for() {
        let said = serde_json::from_slice::<ControlRequest>(br#"{"op":"FromTheFuture","id":"x"}"#)
            .expect_err("unknown")
            .to_string();
        assert!(too_old_for(&said, Some("FromTheFuture")), "{said}");
        assert!(
            !too_old_for("some other refusal", Some("FromTheFuture")),
            "{said}"
        );
    }

    #[test]
    fn an_unknown_op_still_gives_up_its_id() {
        let body = br#"{"op":"FromTheFuture","id":"tree-7"}"#;
        assert!(serde_json::from_slice::<ControlRequest>(body).is_err());
        assert_eq!(raw_request_id(body), "tree-7");
        assert_eq!(raw_request_id(b"not json"), "");
    }

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

    /// The two promises of an unattended move are new: a request without them reads as it
    /// always did (neither), one with neither on the wire is the old bytes, and a daemon from
    /// before them, which ignores keys it does not know, reads the same request without them.
    #[test]
    fn a_restart_ask_without_the_promises_is_the_old_ask() {
        let old: ControlRequest =
            serde_json::from_str(r#"{"op":"RestartWhenSafe","id":"u","version":"1.0.0"}"#)
                .expect("decode");
        assert!(matches!(
            old,
            ControlRequest::RestartWhenSafe {
                keep_tabs: false,
                quiet: false,
                same_version: false,
                ..
            }
        ));
        let plain = serde_json::to_string(&old).expect("encode");
        assert!(
            !plain.contains("keep_tabs") && !plain.contains("quiet") && !plain.contains("same"),
            "{plain}"
        );
        let kept = ControlRequest::RestartWhenSafe {
            id: "u".into(),
            version: "1.0.0".into(),
            keep_tabs: true,
            quiet: true,
            same_version: true,
        };
        let wire = serde_json::to_string(&kept).expect("encode");
        assert!(wire.contains(r#""keep_tabs":true"#) && wire.contains(r#""quiet":true"#));
        assert!(wire.contains(r#""same_version":true"#), "{wire}");
        assert_eq!(
            serde_json::from_str::<ControlRequest>(&wire).expect("decode"),
            kept
        );
        // What an older daemon does with a key it does not know.
        let ignored: ControlRequest = serde_json::from_str(
            r#"{"op":"ListWorkspaces","id":"x","keep_tabs":true,"quiet":true}"#,
        )
        .expect("an unknown key is ignored");
        assert!(matches!(ignored, ControlRequest::ListWorkspaces { .. }));
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
            parked: false,
            news: false,
            keep_awake: false,
            qass_skip: false,
            background: 0,
            subagents: 0,
            task: None,
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
            activity: None,
            pgid: None,
            discards: None,
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

    /// A row's task crosses old daemons both ways: absent from an older daemon's row, kept
    /// from a newer one's, and a row with none writes no key at all.
    #[test]
    fn the_task_field_parses_both_ways() {
        let old: AgentRow =
            serde_json::from_str(r#"{"terminal":1,"workspace":1,"state":"idle"}"#).expect("decode");
        assert_eq!(old.task, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("\"task\""));
        let new: AgentRow = serde_json::from_str(
            r#"{"terminal":1,"workspace":1,"state":"idle","task":{"id":3,"goal":"g","stage":"planning","claimed":true,"later":1}}"#,
        )
        .expect("decode");
        assert_eq!(
            new.task,
            Some(TaskRef {
                id: 3,
                goal: "g".into(),
                stage: "planning".into(),
                claimed: true
            })
        );
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
                from: None,
                id: "d".into(),
                terminal: 3,
                token: "qoop-3-abc".into(),
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
                from: None,
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
        // Secrets a command line carries are masked where every label enters.
        assert_eq!(
            truncate_focus_label("Bash curl -H \"Authorization: Bearer ghx123\" https://x"),
            "Bash curl -H \"Authorization: Bearer *** https://x"
        );
        assert_eq!(
            truncate_focus_label("Bash OPENAI_API_KEY=sk-abc npm test"),
            "Bash OPENAI_API_KEY=*** npm test"
        );
        assert_eq!(
            truncate_focus_label("Bash gh auth ghp_abcdef"),
            "Bash gh auth ***"
        );
        assert_eq!(truncate_focus_label("Edit src/key.rs"), "Edit src/key.rs");
        assert_eq!(
            truncate_focus_label("Bash deploy --token abc123 https://u:pw@h.io/x?api_key=k1&a=2"),
            "Bash deploy --token *** https://u:***@h.io/x?api_key=***&a=2"
        );
        // An escape in a command is text on the way out, never a sequence.
        assert_eq!(
            truncate_focus_label("Bash \x1b]52;c;aGk=\x07 ls"),
            "Bash ]52;c;aGk= ls"
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

    /// A picked colour crosses the wire; a row from an older daemon reads as auto, and a
    /// row with no pick says nothing about it, so an older client sees the row it knew.
    #[test]
    fn a_space_colour_crosses_the_wire_and_its_absence_is_auto() {
        let old: WorkspaceRow =
            serde_json::from_str(r#"{"id":1,"name":"w","terminals":0}"#).expect("decode");
        assert_eq!(old.ink, None);
        let picked = WorkspaceRow {
            ink: Some(4),
            ..old.clone()
        };
        let text = serde_json::to_string(&picked).expect("encode");
        assert_eq!(
            serde_json::from_str::<WorkspaceRow>(&text).expect("decode"),
            picked
        );
        let text = serde_json::to_string(&old).expect("encode");
        assert!(!text.contains("ink"), "{text}");
        assert!(!text.contains("link"), "{text}");
        let linked = WorkspaceRow {
            link: Some(LinkState::Parked),
            ..old.clone()
        };
        let text = serde_json::to_string(&linked).expect("encode");
        assert_eq!(
            serde_json::from_str::<WorkspaceRow>(&text).expect("decode"),
            linked
        );

        for ink in [Some(2), None] {
            let request = ControlRequest::SetWorkspaceInk {
                id: "ink-1".into(),
                workspace: 3,
                ink,
            };
            let encoded = serde_json::to_vec(&request).expect("encode");
            assert_eq!(
                serde_json::from_slice::<ControlRequest>(&encoded).expect("decode"),
                request
            );
            assert_eq!(request.request_id(), "ink-1");
        }
    }

    #[test]
    fn a_scrollback_search_round_trips_and_a_miss_carries_no_row_on_the_wire() {
        let request = ControlRequest::SearchScrollback {
            id: "search-1".into(),
            terminal: 3,
            query: "ERROR".into(),
            from: 70_000,
            backward: true,
        };
        let encoded = serde_json::to_vec(&request).expect("encode");
        let decoded: ControlRequest = serde_json::from_slice(&encoded).expect("decode");
        assert_eq!(request, decoded);
        let miss = ControlResponse::ScrollbackMatch {
            id: "search-1".into(),
            terminal: 3,
            row: None,
            col: 0,
            resume: Some(12_000),
        };
        assert_eq!(request.request_id(), miss.request_id());
        let text = serde_json::to_string(&miss).expect("encode");
        assert!(
            !text.contains("\"row\""),
            "an absent row is left off: {text}"
        );
        let decoded: ControlResponse = serde_json::from_str(&text).expect("decode");
        assert_eq!(miss, decoded);
    }

    #[test]
    fn only_the_seven_reportable_states_parse_and_uncertain_does_not() {
        use qoop_domain::AgentRuntimeState as S;
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

    /// The session history survives the wire, and a client that names no range asks for all.
    #[test]
    fn the_session_history_round_trips_and_an_old_client_leaves_the_range_out() {
        fn back<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: T) {
            let wire = serde_json::to_vec(&value).expect("encode");
            assert_eq!(serde_json::from_slice::<T>(&wire).expect("decode"), value);
        }
        back(ControlRequest::AgentSessions {
            id: "s".into(),
            dirs: vec!["/w/a".into()],
            from_ms: Some(1),
            to_ms: Some(2),
            limit: 8,
        });
        back(ControlResponse::AgentSessions {
            id: "s".into(),
            reading: false,
            sessions: vec![SessionRow {
                key: "/p/x.jsonl".into(),
                agent: "claude".into(),
                id: "x".into(),
                cwd: Some("/w/a".into()),
                title: Some("fix it".into()),
                started_at: Some(1),
                updated_at: Some(2),
                tokens: 30,
                prompts: 2,
                resume: Some("claude --resume x".into()),
                gone: false,
            }],
        });
        back(ControlResponse::AgentActivity {
            id: "a".into(),
            reading: true,
            days: vec![qoop_parsed::index::Day {
                day: "2026-10-20".into(),
                tokens: 5,
                prompts: 1,
                sessions: 1,
                projects: Vec::new(),
                agents: Vec::new(),
            }],
        });
        let bare = br#"{"op":"AgentSessions","id":"s","limit":3}"#;
        assert!(matches!(
            serde_json::from_slice(bare),
            Ok(ControlRequest::AgentSessions {
                from_ms: None,
                to_ms: None,
                ..
            })
        ));
    }

    /// The chat requests, answers and news survive the wire, and what an older client or
    /// daemon does not know degrades to the refusal the clients already handle.
    #[test]
    fn the_chat_messages_round_trip_and_an_old_peer_refuses_them_by_name() {
        fn back<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: T) {
            let wire = serde_json::to_vec(&value).expect("encode");
            assert_eq!(serde_json::from_slice::<T>(&wire).expect("decode"), value);
        }
        back(ControlRequest::Messages {
            id: "m".into(),
            terminal: 3,
            skip: 60,
            limit: 30,
            revision: Some(qoop_parsed::REVISION_MASK),
        });
        back(ControlRequest::MessageDetail {
            id: "d".into(),
            terminal: 3,
            tool_id: "toolu_1".into(),
            max_bytes: Some(4096),
        });
        back(ControlResponse::Messages {
            id: "m".into(),
            revision: qoop_parsed::REVISION_MASK,
            reset: true,
            first_seq: 7,
            has_older: true,
            items: vec![
                qoop_parsed::Item::User {
                    seq: 7,
                    ts: Some(1),
                    text: "hi".into(),
                    attachments: Vec::new(),
                    queued: false,
                },
                qoop_parsed::Item::Compaction { seq: 8, text: None },
            ],
            warnings: 2,
        });
        back(ControlResponse::MessageDetail {
            id: "d".into(),
            input: "in".into(),
            output: "out".into(),
            truncated: true,
        });
        back(EventMessage::MessagesChanged {
            terminal: 3,
            revision: 9,
        });
        // A client that predates `revision` and `max_bytes` leaves them out.
        let old = br#"{"op":"Messages","id":"m","terminal":3,"skip":0,"limit":5}"#;
        assert!(matches!(
            serde_json::from_slice(old),
            Ok(ControlRequest::Messages { revision: None, .. })
        ));
        let old = br#"{"op":"MessageDetail","id":"d","terminal":3,"tool_id":"t"}"#;
        assert!(matches!(
            serde_json::from_slice(old),
            Ok(ControlRequest::MessageDetail {
                max_bytes: None,
                ..
            })
        ));
        // A daemon that does not have the variant answers `unknown variant`, which both
        // interfaces take for "too old" and fall back to the terminal on.
        #[derive(Debug, Deserialize)]
        #[serde(tag = "op")]
        #[allow(dead_code)]
        enum Before {
            Scrollback { id: String },
        }
        let asked = serde_json::to_vec(&ControlRequest::Messages {
            id: "m".into(),
            terminal: 1,
            skip: 0,
            limit: 1,
            revision: None,
        })
        .expect("encode");
        let refusal = serde_json::from_slice::<Before>(&asked)
            .expect_err("an older daemon cannot read it")
            .to_string();
        assert!(too_old_for(&refusal, Some("Messages")), "{refusal}");
        assert!(!too_old_for(&refusal, Some("MessageDetail")), "{refusal}");
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
    /// Show this terminal, asked for by `qoop focus`. An interface switches to it as if
    /// its row were clicked; every other client ignores it, and so does an older
    /// interface, the event lane being lenient.
    Focus { terminal: TerminalStream },
    /// Some news for this connection was dropped before it arrived. Re-read the whole
    /// index rather than waiting for the next change: the missed change may be the last
    /// one for a while, and waiting would leave the client behind silently.
    ///
    /// An older client that does not know this variant ignores it (the event lane
    /// decodes leniently by design), which degrades to the old behaviour of waiting
    /// for the next change rather than breaking the connection.
    Resync,
    /// A task, a board item or a space goal changed: a board reads them again. Sent only
    /// when a write changed something. An older client drops it, the event lane being
    /// lenient.
    TasksChanged,
    /// The question inbox changed: a card came, was answered or ended. Says where to look,
    /// like `AgentChanged`; `AskRead` says what is there. An older client ignores it.
    AsksChanged,
    /// The chat of `terminal` has a new `revision`: its session file grew. Sent only for a
    /// terminal a client asked about in the last 30 seconds, at most once a second; says
    /// where to look, and `Messages` says what is there. An older client ignores it, the
    /// event lane being lenient.
    MessagesChanged {
        terminal: TerminalStream,
        revision: u64,
    },
    /// A result was shown: `ShowRead` says what is there. An older client ignores it.
    ShowsChanged,
    /// A schedule was added, changed, removed or run: `ScheduleRead` says what is there. An
    /// older client ignores it.
    SchedulesChanged,
    /// A program in `terminal` is recording (`on`), and will say so every second while it
    /// does, or has stopped. `listener` numbers the recording. An interface lends its
    /// microphone only right after a key the person pressed there; every other client, and
    /// an older interface, ignores it. See `microphone`.
    Listen {
        terminal: TerminalStream,
        listener: u64,
        on: bool,
    },
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
            said.contains("`qoop stop`"),
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

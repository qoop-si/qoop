//! What a supervising agent needs beyond reading and prompting: hearing that a
//! terminal changed, starting and ending terminals, pressing named keys, and
//! refusing to write to a process it did not read.
//!
//! Every verb here is a client of something the daemon already serves: the
//! event lane, `ListAgents`, `CreateTerminal`, `CloseTerminal` and the
//! terminal input lane. None of them adds a control method.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use qoro_runtime::client;
use qoro_runtime::control::{AgentRow, ControlRequest, ControlResponse, EventMessage};
use qoro_runtime::daemon::ClientWire;
use qoro_runtime::paths;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// States a terminal can be waited for: the index's own closed set.
const STATES: &[&str] = &[
    "starting",
    "idle",
    "working",
    "blocked",
    "done",
    "failed",
    "uncertain",
    "unknown",
    "sleeping",
];

/// What `wait` waits for when not told: every state that means "look now".
const DEFAULT_UNTIL: &[&str] = &["idle", "done", "blocked", "failed"];

/// The exit status of a wait that ran out of time, the same as `timeout(1)`.
const TIMED_OUT: i32 = 124;

/// How long `new` waits for a fresh shell before typing its command.
const SHELL_PATIENCE: Duration = Duration::from_secs(10);

/// How long `close` waits for the index to stop listing a closed terminal.
/// Past the daemon's own teardown grace, which ends with a kill.
const CLOSE_PATIENCE: Duration = Duration::from_secs(10);

/// The gap between two named keys, so `esc` then `enter` does not arrive as
/// one alt-enter in an agent's key parser.
const KEY_GAP: Duration = Duration::from_millis(50);

/// One connection to this session's daemon, held for the whole command.
///
/// One and not one per question, because `wait` depends on it: the daemon
/// starts a connection's event pump when it accepts it, so a change made
/// between a listing and the wait that follows is still delivered on the
/// same connection. A second connection would open a window where it is not.
struct Link {
    rd: tokio::net::unix::OwnedReadHalf,
    wr: tokio::net::unix::OwnedWriteHalf,
    inbuf: bytes::BytesMut,
    connection: client::Connection,
    seq: u32,
    input_seq: BTreeMap<u32, u32>,
}

impl Link {
    async fn open(who: &str) -> std::io::Result<Self> {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        let (stream, inbuf) = client::connect(&socket, who).await?;
        let (rd, wr) = stream.into_split();
        Ok(Self {
            rd,
            wr,
            inbuf,
            connection: client::Connection::new(),
            seq: 0,
            input_seq: BTreeMap::new(),
        })
    }

    /// One control round trip, plus whatever else arrived while waiting.
    async fn ask(
        &mut self,
        request: &ControlRequest,
    ) -> std::io::Result<(ControlResponse, Vec<client::Incoming>)> {
        super::control_roundtrip(
            &mut self.rd,
            &mut self.wr,
            &mut self.inbuf,
            &mut self.connection,
            &mut self.seq,
            request,
            Duration::from_secs(10),
        )
        .await
    }

    /// The live index, plus whatever else arrived while asking for it.
    async fn agents(&mut self) -> std::io::Result<(Vec<AgentRow>, Vec<client::Incoming>)> {
        match self
            .ask(&ControlRequest::ListAgents {
                id: super::request_id("agents"),
            })
            .await?
        {
            (ControlResponse::Agents { agents, .. }, stashed) => Ok((agents, stashed)),
            (ControlResponse::Failed { reason, .. }, _) => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other(
                "unexpected answer to the agent listing",
            )),
        }
    }

    /// Bytes into a terminal on the input lane, signed like `send` signs them.
    async fn input(&mut self, terminal: u32, bytes: Vec<u8>) -> std::io::Result<()> {
        let (from_terminal, from_token) = super::cli_sender();
        let seq = self.input_seq.entry(terminal).or_insert(0);
        let frame = client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes,
                from_terminal,
                from_token,
            },
            seq,
        )?;
        self.wr.write_all(&frame).await?;
        self.wr.flush().await
    }

    /// Read until an event arrives, acknowledging renders on the way.
    ///
    /// Renders are acknowledged because the daemon disconnects a reader that
    /// holds unacknowledged ones past `NO_ACK_DEADLINE`, and a connection is
    /// subscribed to a terminal the moment it is accepted. `None` at the
    /// deadline.
    async fn next_events(
        &mut self,
        deadline: Option<Instant>,
    ) -> std::io::Result<Option<Vec<EventMessage>>> {
        let mut read = [0u8; 64 * 1024];
        loop {
            if let Some(ack) = self.connection.renders().due_ack(Instant::now()) {
                self.wr.write_all(&ack).await?;
            }
            let tick = Duration::from_secs(1);
            let wait = match deadline {
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(None);
                    }
                    left.min(tick)
                }
                None => tick,
            };
            let n = match tokio::time::timeout(wait, self.rd.read(&mut read)).await {
                Err(_) => continue,
                Ok(n) => n?,
            };
            if n == 0 {
                return Err(std::io::Error::other("the daemon closed the connection"));
            }
            self.inbuf.extend_from_slice(&read[..n]);
            let events: Vec<EventMessage> = self
                .connection
                .drain(&mut self.inbuf)?
                .into_iter()
                .filter_map(|incoming| match incoming {
                    client::Incoming::Event(event) => Some(event),
                    _ => None,
                })
                .collect();
            if !events.is_empty() {
                return Ok(Some(events));
            }
        }
    }
}

/// Whether any of these arrivals says a watched terminal may have changed.
fn touches(events: &[EventMessage], watched: &[u32]) -> bool {
    events.iter().any(|event| match event {
        EventMessage::AgentChanged { terminal } => watched.contains(terminal),
        EventMessage::Resync => true,
    })
}

/// The same, over what a control round trip set aside.
fn stashed_touches(stashed: Vec<client::Incoming>, watched: &[u32]) -> bool {
    let events: Vec<EventMessage> = stashed
        .into_iter()
        .filter_map(|incoming| match incoming {
            client::Incoming::Event(event) => Some(event),
            _ => None,
        })
        .collect();
    touches(&events, watched)
}

/// Why a write must not go to this terminal under this generation, if not.
///
/// The generation is the process, not the state: a write checked against
/// the generation read before it cannot land on the agent that replaced the
/// one that was read.
pub(crate) fn generation_refusal(rows: &[AgentRow], terminal: u32, want: u64) -> Option<String> {
    let Some(row) = rows.iter().find(|r| r.terminal == terminal) else {
        return Some(format!("no such terminal: {terminal}"));
    };
    match row.generation {
        Some(g) if g == want => None,
        Some(g) => Some(format!(
            "terminal {terminal} is generation {g} now, not {want}: read it again before writing"
        )),
        None => Some(format!(
            "terminal {terminal} has no generation yet, not {want}: read it again before writing"
        )),
    }
}

/// Take a trailing `--generation N` off a command's arguments.
///
/// Only as the last two words: a prompt's text is arguments too, and
/// `qoro prompt 3 explain --generation 5 to me` is a sentence about the flag,
/// not the flag. Anything that is not a number after it is left as text.
pub(crate) fn take_generation(args: &mut Vec<String>) -> Option<u64> {
    let [.., flag, value] = args.as_slice() else {
        return None;
    };
    if flag != "--generation" {
        return None;
    }
    let value = value.parse::<u64>().ok()?;
    args.truncate(args.len() - 2);
    Some(value)
}

/// Refuse a write whose generation moved, over a connection of its own.
///
/// For `send` and `prompt`, which predate this and open their own. Nothing
/// is asked when no generation was given.
pub(crate) fn check_generation(terminal: u32, want: Option<u64>) -> std::io::Result<()> {
    let Some(want) = want else {
        return Ok(());
    };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (rows, _) = Link::open("qoro-generation").await?.agents().await?;
        match generation_refusal(&rows, terminal, want) {
            Some(reason) => Err(std::io::Error::other(reason)),
            None => Ok(()),
        }
    })
}

/// The terminal this command runs in, when it runs in one of this session.
///
/// Both halves, because terminal numbers repeat across sessions: tab 2 of
/// another session is not this tab. `home` is the session this process was
/// started in, read before `--session` replaced it.
fn own_terminal(home: Option<&str>, session: &str) -> Option<u32> {
    let terminal = std::env::var("QORO_TERMINAL").ok()?.parse().ok()?;
    (home == Some(session)).then_some(terminal)
}

/// `qoro wait`: block until a terminal reaches a state.
///
/// Level triggered: a
/// target already there returns at once. Exits 124 on timeout, 1 when a
/// target closes or nothing can be waited on.
pub(crate) fn run_wait(args: &[String], home: Option<&str>, session: &str) -> std::io::Result<()> {
    let usage = "usage: qoro wait [target...] [--until STATE[,STATE...]] [--timeout SECS] [--json]";
    let mut targets = Vec::new();
    let mut until: Vec<String> = Vec::new();
    let mut timeout = None;
    let mut json = false;
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--until" => {
                let value = rest.next().ok_or_else(|| std::io::Error::other(usage))?;
                for state in value.split(',').filter(|s| !s.is_empty()) {
                    if !STATES.contains(&state) {
                        return Err(std::io::Error::other(format!(
                            "unknown state {state}; one of {}",
                            STATES.join(", ")
                        )));
                    }
                    until.push(state.to_string());
                }
            }
            "--timeout" => {
                let secs: u64 = rest
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| std::io::Error::other(usage))?;
                timeout = Some(Duration::from_secs(secs));
            }
            flag if flag.starts_with("--") => {
                return Err(std::io::Error::other(format!(
                    "unknown flag {flag}; {usage}"
                )));
            }
            target => targets.push(target.to_string()),
        }
    }
    if until.is_empty() {
        until = DEFAULT_UNTIL.iter().map(|s| s.to_string()).collect();
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    let own = own_terminal(home, session);

    let rt = tokio::runtime::Runtime::new()?;
    let matched = rt.block_on(async {
        let mut link = Link::open("qoro-wait").await?;
        let (rows, _) = link.agents().await?;
        let watched: Vec<u32> = if targets.is_empty() {
            rows.iter()
                .filter(|r| r.agent && Some(r.terminal) != own)
                .map(|r| r.terminal)
                .collect()
        } else {
            let mut out = Vec::new();
            for target in &targets {
                let row = super::resolve_target(&rows, target).map_err(std::io::Error::other)?;
                out.push(row.terminal);
            }
            out
        };
        if watched.is_empty() {
            return Err(std::io::Error::other(
                "no agent terminals to wait on; name a target",
            ));
        }
        // Renders are not what this waits on. Dropping the subscription the
        // daemon made at accept keeps a long wait from being paced by output.
        let all = rows.iter().map(|r| r.terminal).collect();
        link.ask(&ControlRequest::Unsubscribe {
            id: super::request_id("unsubscribe"),
            terminals: all,
        })
        .await?;
        loop {
            let (rows, stashed) = link.agents().await?;
            // A change that arrived while this listing was in flight may be
            // newer than the listing: look again before deciding.
            if stashed_touches(stashed, &watched) {
                continue;
            }
            if let Some(gone) = watched
                .iter()
                .find(|t| !rows.iter().any(|r| r.terminal == **t))
            {
                return Err(std::io::Error::other(format!("terminal {gone} closed")));
            }
            let hits: Vec<AgentRow> = rows
                .into_iter()
                .filter(|r| watched.contains(&r.terminal) && until.contains(&r.state))
                .collect();
            if !hits.is_empty() {
                return Ok(Some(hits));
            }
            loop {
                match link.next_events(deadline).await? {
                    None => return Ok(None),
                    Some(events) if touches(&events, &watched) => break,
                    Some(_) => {}
                }
            }
        }
    })?;
    let Some(rows) = matched else {
        eprintln!(
            "qoro: timed out after {}s waiting for {}",
            timeout.map(|t| t.as_secs()).unwrap_or(0),
            until.join(",")
        );
        std::process::exit(TIMED_OUT);
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(std::io::Error::other)?
        );
    } else {
        for row in &rows {
            println!("{}", super::agent_line(row));
        }
    }
    Ok(())
}

/// `qoro new`: open a terminal, optionally typing a command into its shell.
///
/// Prints only the terminal number, so `t=$(qoro new ...)` works. The
/// command is typed rather than spawned, the same path a person takes: the
/// daemon starts the user's shell and nothing else (ADR-015), and the shell
/// gives the command its environment.
pub(crate) fn run_new(args: &[String]) -> std::io::Result<()> {
    let usage = "usage: qoro new [--name NAME] [--cwd DIR] [--space ID] [-- command...]";
    let mut name = None;
    let mut cwd = None;
    let mut space = None;
    let mut command: Vec<String> = Vec::new();
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| std::io::Error::other(usage))
        };
        match arg.as_str() {
            "--name" => name = Some(value()?),
            "--cwd" => cwd = Some(value()?),
            "--space" => {
                space = Some(
                    value()?
                        .parse::<i64>()
                        .map_err(|_| std::io::Error::other(usage))?,
                )
            }
            "--" => {
                command = rest.by_ref().cloned().collect();
                break;
            }
            other => {
                return Err(std::io::Error::other(format!(
                    "unexpected {other}; {usage}"
                )));
            }
        }
    }
    // Where the caller is, not where the daemon started: a person opening a
    // tab from here expects to land here. Absolute, because the daemon
    // refuses a relative path rather than guess what it is relative to.
    let here = std::env::current_dir()?;
    let cwd = match cwd {
        Some(dir) => std::path::absolute(here.join(dir))?,
        None => here,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = Link::open("qoro-new").await?;
        let terminal = match link
            .ask(&ControlRequest::CreateTerminal {
                id: super::request_id("new"),
                cwd: Some(cwd.display().to_string()),
                workspace: space,
                name,
            })
            .await?
            .0
        {
            ControlResponse::TerminalCreated { terminal, .. } => terminal,
            ControlResponse::Failed { reason, .. } => return Err(std::io::Error::other(reason)),
            _ => return Err(std::io::Error::other("unexpected answer to a create")),
        };
        println!("{terminal}");
        if command.is_empty() {
            return Ok(());
        }
        // Typed once the shell has drawn something and gone quiet: a process
        // the daemon can name is not yet a line editor reading its input, and
        // a slow rc file runs in between. `fetch_snapshot` settles for half a
        // second, which is the quiet part.
        let deadline = Instant::now() + SHELL_PATIENCE;
        loop {
            let screen = super::fetch_snapshot(terminal, "qoro-new").await?;
            if screen.lines.iter().any(|l| !l.trim().is_empty()) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::other(format!(
                    "terminal {terminal} opened but its shell drew nothing within {}s; the command was not typed",
                    SHELL_PATIENCE.as_secs()
                )));
            }
        }
        let line: Vec<String> = command.iter().map(|w| shell_quote(w)).collect();
        link.input(terminal, format!("{}\r", line.join(" ")).into_bytes())
            .await
    })
}

/// `qoro close`: end a terminal and what it started, as `ctrl-a x` does.
///
/// A number and never a name: a destructive verb does not guess. The
/// terminal this runs in is refused, because a supervisor that closes
/// itself has lost the loop it was running.
pub(crate) fn run_close(args: &[String], home: Option<&str>, session: &str) -> std::io::Result<()> {
    let usage = "usage: qoro close <terminal> [--generation G]";
    let mut args = args.to_vec();
    let generation = take_generation(&mut args);
    let terminal: u32 = match args.get(2..) {
        Some([n]) => n.parse().map_err(|_| std::io::Error::other(usage))?,
        _ => return Err(std::io::Error::other(usage)),
    };
    if own_terminal(home, session) == Some(terminal) {
        return Err(std::io::Error::other(
            "refusing to close the terminal this command runs in",
        ));
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = Link::open("qoro-close").await?;
        if let Some(want) = generation {
            let (rows, _) = link.agents().await?;
            if let Some(reason) = generation_refusal(&rows, terminal, want) {
                return Err(std::io::Error::other(reason));
            }
        }
        match link
            .ask(&ControlRequest::CloseTerminal {
                id: super::request_id("close"),
                terminal,
            })
            .await?
            .0
        {
            ControlResponse::TerminalClosed { .. } => {}
            ControlResponse::Failed { reason, .. } => return Err(std::io::Error::other(reason)),
            _ => return Err(std::io::Error::other("unexpected answer to a close")),
        }
        // The daemon answers when the close is accepted, and the teardown
        // that contains what the terminal started runs after (ADR-014). Said
        // "closed" only once the index agrees, so a supervisor that lists
        // next does not find the terminal it was just told is gone.
        let deadline = Instant::now() + CLOSE_PATIENCE;
        loop {
            let (rows, _) = link.agents().await?;
            if !rows.iter().any(|r| r.terminal == terminal) {
                println!("closed terminal {terminal}");
                return Ok(());
            }
            let tick = (Instant::now() + Duration::from_millis(250)).min(deadline);
            if link.next_events(Some(tick)).await?.is_none() && Instant::now() >= deadline {
                return Err(std::io::Error::other(format!(
                    "terminal {terminal} accepted the close but was still listed after {}s",
                    CLOSE_PATIENCE.as_secs()
                )));
            }
        }
    })
}

/// `qoro send <terminal> --key NAME...`: press named keys, one frame each.
pub(crate) fn send_keys(terminal: u32, keys: &[String]) -> std::io::Result<()> {
    for key in keys {
        if key_bytes(key, false).is_none() {
            return Err(std::io::Error::other(format!(
                "unknown key {key}; one of enter esc tab shift-tab backspace space up down left right home end pageup pagedown ctrl-a..ctrl-z"
            )));
        }
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // The spelling of an arrow depends on what the program asked for, so
        // look only when an arrow is being sent: it costs a screen read.
        let app_cursor = if keys.iter().any(|k| key_needs_mode(k)) {
            super::fetch_snapshot(terminal, "qoro-send")
                .await?
                .modes
                .application_cursor
        } else {
            false
        };
        let mut link = Link::open("qoro-send").await?;
        match link
            .ask(&ControlRequest::ListTerminals {
                id: super::request_id("send"),
            })
            .await?
            .0
        {
            ControlResponse::Terminals { terminals, .. } if terminals.contains(&terminal) => {}
            _ => {
                return Err(std::io::Error::other(format!(
                    "no such terminal: {terminal}"
                )));
            }
        }
        for (i, key) in keys.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(KEY_GAP).await;
            }
            let bytes = key_bytes(key, app_cursor).expect("checked above");
            link.input(terminal, bytes).await?;
        }
        Ok(())
    })
}

/// The bytes one named key sends, or `None` for a name that is not a key.
///
/// `app_cursor` is the target's application cursor mode: arrows, home and end
/// are `SS3` there and `CSI` everywhere else, and the wrong spelling is a key
/// the program ignores (measured against `less`, see `ModesWire`).
pub(crate) fn key_bytes(name: &str, app_cursor: bool) -> Option<Vec<u8>> {
    let cursor = |c: u8| {
        let lead: &[u8] = if app_cursor { b"\x1bO" } else { b"\x1b[" };
        let mut out = lead.to_vec();
        out.push(c);
        out
    };
    let bytes = match name {
        "enter" => b"\r".to_vec(),
        "esc" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "shift-tab" => b"\x1b[Z".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "space" => b" ".to_vec(),
        "up" => cursor(b'A'),
        "down" => cursor(b'B'),
        "right" => cursor(b'C'),
        "left" => cursor(b'D'),
        "home" => cursor(b'H'),
        "end" => cursor(b'F'),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        _ => {
            let letter = name.strip_prefix("ctrl-")?;
            let [c] = letter.as_bytes() else {
                return None;
            };
            if !c.is_ascii_lowercase() {
                return None;
            }
            vec![c & 0x1f]
        }
    };
    Some(bytes)
}

/// Whether a key's spelling depends on the application cursor mode.
pub(crate) fn key_needs_mode(name: &str) -> bool {
    matches!(name, "up" | "down" | "right" | "left" | "home" | "end")
}

/// One argv word as a POSIX shell reads it back: bare when it is plainly
/// safe, single quoted otherwise, with an embedded quote closed and reopened.
pub(crate) fn shell_quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:=@%+-,".contains(&b));
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_spell_arrows_by_cursor_mode() {
        assert_eq!(key_bytes("up", false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("up", true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes("end", true).unwrap(), b"\x1bOF");
        assert_eq!(key_bytes("esc", true).unwrap(), b"\x1b");
        assert_eq!(key_bytes("enter", false).unwrap(), b"\r");
        assert_eq!(key_bytes("ctrl-c", false).unwrap(), [3]);
        assert_eq!(key_bytes("ctrl-a", false).unwrap(), [1]);
        assert_eq!(key_bytes("ctrl-z", false).unwrap(), [26]);
        for bad in ["ctrl-", "ctrl-C", "ctrl-1", "ctrl-ab", "Enter", "f1", ""] {
            assert_eq!(key_bytes(bad, false), None, "{bad}");
        }
        assert!(key_needs_mode("left") && !key_needs_mode("pageup"));
    }

    #[test]
    fn only_a_trailing_generation_is_the_flag() {
        let words = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let mut args = words("qoro prompt 3 hello --generation 7");
        assert_eq!(take_generation(&mut args), Some(7));
        assert_eq!(args, words("qoro prompt 3 hello"));
        for text in [
            "qoro prompt 3 explain --generation 5 to me",
            "qoro prompt 3 set --generation high",
            "qoro prompt 3 --generation",
        ] {
            let mut args = words(text);
            assert_eq!(take_generation(&mut args), None, "{text}");
            assert_eq!(args, words(text), "the text is left whole");
        }
    }

    #[test]
    fn quoting_round_trips_through_a_real_shell() {
        let words = [
            "claude",
            "fix the bug",
            "it's",
            "",
            "$HOME",
            "a\"b",
            "--x=1",
        ];
        let line: Vec<String> = words.iter().map(|w| shell_quote(w)).collect();
        assert_eq!(line[0], "claude");
        assert_eq!(line[1], "'fix the bug'");
        assert_eq!(line[6], "--x=1");
        let script = format!(
            "for a in {}; do printf '%s\\n' \"$a\"; done",
            line.join(" ")
        );
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .output()
            .expect("run sh");
        let back: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(back, words);
    }
}

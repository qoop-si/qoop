//! What a herdr plugin changes in qoop, through the host: input, names, focus, terminals and
//! spaces, plugin panes, notes and actions.
//!
//! Input goes through `qoop send` and `qoop prompt` themselves, run as children of the host,
//! so a plugin meets every refusal a person's script meets (a person at the tab, a blocked
//! agent, qass's guard) and none is bypassed or retried: a refusal is the plugin's error. The
//! rest maps onto the control requests the interface sends. Where qoop has nothing to change
//! (a pane's zoom is the interface's own), the host keeps what herdr would answer and says so.
//!
//! Each method is an async fn of its own, boxed where it is called: one future holding every
//! method's state is more stack than a runtime worker has in a debug build.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse, Pane, SplitDir};
use serde_json::{Value, json};

use super::host::{self, Host, Popup, Refused, State, not_found, param, terminal_of};
use super::install;
use super::translate::{self, Id};
use crate::supervise::Link;

/// How long a plugin pane's spec waits for its `pane-exec`.
const SPEC_TTL: Duration = Duration::from_secs(30);
/// How long a close waits for the daemon to stop listing what it closed, as `qoop close` does.
const CLOSE_PATIENCE: Duration = Duration::from_secs(10);
/// How long a new shell has to draw its prompt before a line is typed into it.
const SHELL_PATIENCE: Duration = Duration::from_secs(10);
/// How long an agent has to be seen at work after a prompt that is waited on, as in herdr.
const TURN_START: Duration = Duration::from_millis(5000);
/// The longest `agent.prompt` waits for a turn to end.
const WAIT_CAP: Duration = Duration::from_secs(60 * 60);
/// The most text one call types: far beyond a prompt, and well inside what an argv can carry.
const MAX_TEXT: usize = 64 * 1024;

/// What a pane's `pane-exec` becomes, held in the host's memory until it takes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Spec {
    pub(crate) argv: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) cwd: PathBuf,
    /// Leave qoop's own variables behind: true for a plugin's program, false for a shell a
    /// plugin asked for with an environment, which stays the terminal's own shell.
    pub(crate) scrub: bool,
}

/// The specs waiting, by nonce: each taken once, and gone after [`SPEC_TTL`].
#[derive(Clone, Default)]
pub(crate) struct Specs(Arc<Mutex<BTreeMap<String, (Instant, Spec)>>>);

impl Specs {
    /// Keep `spec` under `nonce` (from [`nonce`]); what is past its time goes on the way.
    fn put_at(&self, nonce: &str, spec: Spec) {
        let mut held = self.0.lock().unwrap();
        held.retain(|_, (at, _)| at.elapsed() < SPEC_TTL);
        held.insert(nonce.to_string(), (Instant::now(), spec));
    }

    /// The spec under `nonce`, once: a second take, an unknown nonce or one past its time finds
    /// nothing.
    pub(crate) fn take(&self, nonce: &str) -> Option<Spec> {
        let (at, spec) = self.0.lock().unwrap().remove(nonce)?;
        (at.elapsed() < SPEC_TTL).then_some(spec)
    }
}

/// 128 random bits, as hex.
fn nonce() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The line a pane's shell is handed: the same for every pane but its nonce.
fn exec_line(nonce: &str) -> Result<String, Refused> {
    let exe = std::env::current_exe().map_err(|e| ("failed", e.to_string()))?;
    Ok(format!(
        "exec {} plugin pane-exec {nonce}",
        crate::supervise::shell_quote(&exe.to_string_lossy())
    ))
}

/// Whether `name` can be an environment variable's name.
pub(crate) fn env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A call's `env` object for a new pane (split, tab, space or plugin pane), checked as herdr
/// checks it. What says which plugin and host a process belongs to is dropped: the host sets
/// it itself where it applies.
fn env_param(params: &Value) -> Result<Vec<(String, String)>, Refused> {
    let mut out = Vec::new();
    let refuse = |why: String| Err(("invalid_env", why));
    if let Some(env) = params.get("env").and_then(Value::as_object) {
        for (name, value) in env {
            let value = value.as_str().unwrap_or_default();
            if name.is_empty() {
                return refuse("env key must not be empty".into());
            }
            if name.contains('=') {
                return refuse(format!("env key {name} must not contain '='"));
            }
            if name.contains('\0') {
                return refuse("env key must not contain NUL bytes".into());
            }
            if value.contains('\0') {
                return refuse(format!("env value for {name} must not contain NUL bytes"));
            }
            if name.starts_with("HERDR_PLUGIN_")
                || ["HERDR_SOCKET_PATH", "HERDR_ENV", "HERDR_BIN_PATH"].contains(&name.as_str())
            {
                continue;
            }
            // qoop's own say which terminal and runtime a process belongs to; a plugin never
            // sets them.
            if !env_name(name) || name.starts_with("QOOP_") || name.starts_with("QORO_") {
                return refuse(format!("invalid env name {name:?}"));
            }
            out.push((name.clone(), value.to_string()));
        }
    }
    Ok(out)
}

/// A caller's context over ours, field by field; a null it sends leaves ours, as herdr merges.
fn merge_context(ours: &mut Value, theirs: Option<&Value>) {
    if let (Value::Object(ours), Some(Value::Object(theirs))) = (ours, theirs) {
        for (key, value) in theirs.iter().filter(|(_, v)| !v.is_null()) {
            ours.insert(key.clone(), value.clone());
        }
    }
}

/// A re-started shell's environment: the variables qoop's zsh integration was started with
/// (`ZDOTDIR`, `QOOP_ZDOTDIR`), read from the shell's start environment by `start`, since the
/// running shell has put its own `ZDOTDIR` back by now; then what the plugin asked for.
fn shell_env(
    start: impl Fn(&str) -> Option<String>,
    asked: Vec<(String, String)>,
) -> Vec<(String, String)> {
    ["ZDOTDIR", "QOOP_ZDOTDIR"]
        .into_iter()
        .filter_map(|name| Some((name.to_string(), start(name)?)))
        .chain(asked)
        .collect()
}

/// What one herdr key name presses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Bytes typed as they are.
    Bytes(Vec<u8>),
    /// A key `qoop send --key` presses: a plain arrow, spelt as the program's cursor key mode
    /// asks, as herdr's encoder follows it.
    Key(&'static str),
    /// A key herdr accepts and sends nothing for (a hyper or super chord, a function key past
    /// F25).
    Nothing,
}

/// The `CSI <n> ~` numbers of F5 to F25, in order.
const FUNCTION_TILDE: [u8; 21] = [
    15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45, 46,
];

/// A herdr key name, read as herdr reads it (`ctrl+c`, `C-c`, `alt+up`, `shift+tab`, `F5`,
/// `q`, `plus`), and what it presses in a pane with no keyboard protocol of its own: modified
/// arrows and function keys as `CSI 1;M` sequences, `alt` as an escape before the key, `ctrl`
/// with a letter as its control byte. A name herdr refuses is `invalid_key`, and so is one
/// that would send a NUL byte, which a terminal's input here cannot carry.
pub(crate) fn press(name: &str) -> Result<Press, Refused> {
    let unsupported = || ("invalid_key", format!("unsupported key {name}"));
    let trimmed = name.trim();
    let spelled = match trimmed {
        "C-c" | "c-c" => "ctrl+c",
        "+" => "plus",
        other => other,
    };
    let (mods, key) = match spelled.strip_suffix("++") {
        Some(mods) => (mods, "plus"),
        None => match spelled.rsplit_once('+') {
            Some((mods, key)) => (mods, key),
            None => ("", spelled),
        },
    };
    let (mut ctrl, mut shift, mut alt, mut dropped) = (false, false, false, false);
    for part in mods.split('+').filter(|_| !mods.is_empty()) {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" | "meta" => alt = true,
            // Never sent by herdr to a pane with no keyboard protocol.
            "cmd" | "command" | "super" | "hyper" => dropped = true,
            _ => return Err(unsupported()),
        }
    }
    let key = key.trim();
    if key.is_empty() {
        return Err(unsupported());
    }
    let named = key.to_ascii_lowercase();
    let character = match named.as_str() {
        "minus" => Some('-'),
        "comma" => Some(','),
        "period" => Some('.'),
        "slash" => Some('/'),
        "backslash" => Some('\\'),
        "quote" => Some('\''),
        "double_quote" | "double-quote" => Some('"'),
        "semicolon" => Some(';'),
        "colon" => Some(':'),
        "percent" => Some('%'),
        "ampersand" => Some('&'),
        "backtick" => Some('`'),
        "plus" => Some('+'),
        "space" => Some(' '),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        }
    };
    let code = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let csi = |body: String| format!("\x1b[{body}").into_bytes();
    let pressed = if let Some(c) = character {
        let c = if shift && c.is_ascii_lowercase() {
            c.to_ascii_uppercase()
        } else {
            c
        };
        let byte = if ctrl {
            match c.to_ascii_lowercase() {
                l @ 'a'..='z' => Some(vec![l as u8 & 0x1f]),
                '[' => Some(vec![0x1b]),
                '\\' => Some(vec![0x1c]),
                ']' => Some(vec![0x1d]),
                '/' => Some(vec![0x1f]),
                '8' => Some(vec![0x7f]),
                ' ' | '2' | '@' => return Err(unsupported()),
                _ => None,
            }
        } else {
            None
        };
        byte.unwrap_or_else(|| c.to_string().into_bytes())
    } else {
        let arrow = match named.as_str() {
            "up" => Some('A'),
            "down" => Some('B'),
            "right" => Some('C'),
            "left" => Some('D'),
            _ => None,
        };
        if let Some(letter) = arrow {
            if code == 1 && !dropped {
                return Ok(Press::Key(match letter {
                    'A' => "up",
                    'B' => "down",
                    'C' => "right",
                    _ => "left",
                }));
            }
            csi(format!("1;{code}{letter}"))
        } else if let Some(n) = named.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            match n {
                1..=4 if code == 1 => {
                    format!("\x1bO{}", ['P', 'Q', 'R', 'S'][usize::from(n - 1)]).into_bytes()
                }
                1 | 2 | 4 => csi(format!(
                    "1;{code}{}",
                    ['P', 'Q', 'R', 'S'][usize::from(n - 1)]
                )),
                3 => csi(format!("13;{code}~")),
                5..=25 => {
                    let tilde = FUNCTION_TILDE[usize::from(n - 5)];
                    if code == 1 {
                        csi(format!("{tilde}~"))
                    } else {
                        csi(format!("{tilde};{code}~"))
                    }
                }
                _ => return Ok(Press::Nothing),
            }
        } else {
            match named.as_str() {
                "enter" | "return" => b"\r".to_vec(),
                "esc" | "escape" => b"\x1b".to_vec(),
                "tab" if shift => b"\x1b[Z".to_vec(),
                "tab" => b"\t".to_vec(),
                "backspace" | "bs" if ctrl => vec![0x08],
                "backspace" | "bs" => vec![0x7f],
                _ => return Err(unsupported()),
            }
        }
    };
    if dropped {
        return Ok(Press::Nothing);
    }
    let alt_prefixed = alt && !pressed.starts_with(b"\x1b[") && !pressed.starts_with(b"\x1bO");
    Ok(Press::Bytes(if alt_prefixed {
        [b"\x1b".as_slice(), &pressed].concat()
    } else {
        pressed
    }))
}

/// The pane a call without a target is about: `from`, or, when that is a popup a plugin
/// opened, the pane that was focused before it.
pub(crate) fn default_target(from: Option<u32>, popups: &BTreeMap<u32, Popup>) -> Option<u32> {
    match from {
        Some(t) if popups.contains_key(&t) => popups[&t].previous,
        other => other,
    }
}

/// `tree` with the leaf `target` replaced by `with`.
fn replace_leaf(tree: &Pane, target: u32, with: &Pane) -> Pane {
    match tree {
        Pane::Leaf { terminal } if *terminal == target => with.clone(),
        Pane::Leaf { .. } => tree.clone(),
        Pane::Split {
            dir,
            permille,
            questions,
            first,
            second,
        } => Pane::Split {
            dir: *dir,
            permille: *permille,
            questions: *questions,
            first: Box::new(replace_leaf(first, target, with)),
            second: Box::new(replace_leaf(second, target, with)),
        },
    }
}

/// Whether a note may be shown, and herdr's reason: muted here or everywhere is `disabled`.
pub(crate) fn may_show(mute: qoop_runtime::config::Mute) -> (bool, &'static str) {
    match mute {
        qoop_runtime::config::Mute::Off => (true, "shown"),
        _ => (false, "disabled"),
    }
}

/// herdr's code for what `qoop send` or `qoop prompt` said when it refused.
pub(crate) fn refusal_code(said: &str) -> &'static str {
    if said.contains("no such terminal") {
        "pane_not_found"
    } else if said.contains("is blocked") {
        "agent_blocked"
    } else if said.contains("trust prompt") || said.contains("is sleeping") {
        "agent_not_ready"
    } else if said.contains("not an agent") {
        "agent_not_found"
    } else if said.contains("nothing is awake") {
        "server_not_running"
    } else {
        "input_refused"
    }
}

/// One control request on a connection of its own; a `Failed` answer is the plugin's error.
pub(super) async fn control(request: ControlRequest) -> Result<ControlResponse, Refused> {
    let unavailable = |e: std::io::Error| ("server_not_running", e.to_string());
    let mut link = Link::open_observer("qoop-plugin-host")
        .await
        .map_err(unavailable)?;
    match link.ask(&request).await.map_err(unavailable)?.0 {
        ControlResponse::Failed { reason, .. } => Err(("failed", reason)),
        other => Ok(other),
    }
}

/// A new terminal in `workspace`, its number back. The connection that made it is
/// subscribed to it by the daemon, so it unsubscribes before it goes.
async fn create(workspace: i64, cwd: Option<String>, name: Option<String>) -> Result<u32, Refused> {
    let unavailable = |e: std::io::Error| ("server_not_running", e.to_string());
    let mut link = Link::open_observer("qoop-plugin-host")
        .await
        .map_err(unavailable)?;
    let made = ControlRequest::CreateTerminal {
        id: crate::request_id("plugin-host"),
        cwd,
        workspace: Some(workspace),
        name,
    };
    let terminal = match link.ask(&made).await.map_err(unavailable)?.0 {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        ControlResponse::Failed { reason, .. } => return Err(("failed", reason)),
        _ => return Err(("failed", "unexpected answer to a create".into())),
    };
    let leave = ControlRequest::Unsubscribe {
        id: crate::request_id("plugin-host"),
        terminals: vec![terminal],
    };
    let _ = link.ask(&leave).await;
    Ok(terminal)
}

/// Wait until `terminal` has drawn something: its shell has started and taken over the terminal.
async fn drawn(terminal: u32) -> Result<(), Refused> {
    let unavailable = |e: std::io::Error| ("server_not_running", e.to_string());
    let deadline = Instant::now() + SHELL_PATIENCE;
    loop {
        let screen = crate::fetch_snapshot(terminal, "qoop-plugin-host")
            .await
            .map_err(unavailable)?;
        if screen.lines.iter().any(|l| !l.trim().is_empty()) {
            break;
        }
        if Instant::now() >= deadline {
            return Err((
                "failed",
                format!("terminal {terminal} drew nothing to type into"),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

/// Type `line` and Enter into `terminal` now; a new terminal waits for [`drawn`] first.
async fn type_line(terminal: u32, line: &str) -> Result<(), Refused> {
    let unavailable = |e: std::io::Error| ("server_not_running", e.to_string());
    let mut link = Link::open_observer("qoop-plugin-host")
        .await
        .map_err(unavailable)?;
    link.input(terminal, format!("{line}\r").into_bytes())
        .await
        .map_err(unavailable)
}

/// `qoop <args>` as a child of the host: its refusal (a person at the tab, a blocked agent,
/// qass's guard) is the error, in herdr's code where one fits and worded as the command words it.
async fn qoop(args: Vec<String>) -> Result<(), Refused> {
    let exe = std::env::current_exe().map_err(|e| ("failed", e.to_string()))?;
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
    })
    .await
    .map_err(|e| ("failed", e.to_string()))?
    .map_err(|e| ("failed", e.to_string()))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.trim().trim_start_matches("qoop: ").to_string();
    Err((refusal_code(&said), said))
}

/// What a write types, in order: a paste (inside the bracketed paste markers when the program
/// asked for them), then key presses.
fn typed(paste: &str, keys: &[Value]) -> Result<Vec<Press>, Refused> {
    if paste.len() > MAX_TEXT {
        return Err((
            "invalid_request",
            format!("invalid request: text over {MAX_TEXT} bytes"),
        ));
    }
    let mut presses = Vec::new();
    for key in keys {
        presses.push(press(key.as_str().unwrap_or_default())?);
    }
    Ok(presses)
}

/// Type into `terminal` through `qoop send`, so every refusal it makes is the plugin's: `text`
/// as it is (`paste` puts it inside the bracketed paste markers when the program asked for
/// them), then the presses, bytes as text and plain arrows as `--key`, in order. Nothing to
/// type sends nothing.
async fn send(terminal: u32, text: &str, paste: bool, presses: Vec<Press>) -> Result<(), Refused> {
    let t = terminal.to_string();
    if !text.is_empty() {
        let mut args = vec!["send".to_string(), t.clone()];
        if paste {
            args.push("--paste".into());
        }
        args.extend(["--".to_string(), text.to_string()]);
        qoop(args).await?;
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut keys: Vec<&str> = Vec::new();
    for press in presses.iter().chain([&Press::Nothing]) {
        let flush_bytes = !matches!(press, Press::Bytes(_)) && !bytes.is_empty();
        let flush_keys = !matches!(press, Press::Key(_)) && !keys.is_empty();
        if flush_bytes {
            let text = String::from_utf8(std::mem::take(&mut bytes))
                .map_err(|_| ("invalid_key", "unsupported key".to_string()))?;
            qoop(vec!["send".into(), t.clone(), "--".into(), text]).await?;
        }
        if flush_keys {
            let mut args = vec!["send".to_string(), t.clone()];
            for key in std::mem::take(&mut keys) {
                args.extend(["--key".to_string(), key.to_string()]);
            }
            qoop(args).await?;
        }
        match press {
            Press::Bytes(b) => bytes.extend(b),
            Press::Key(k) => keys.push(k),
            Press::Nothing => {}
        }
    }
    Ok(())
}

fn bool_param(params: &Value, name: &str, default: bool) -> bool {
    params.get(name).and_then(Value::as_bool).unwrap_or(default)
}

/// The terminal `id` names: a pane id or terminal number, or for an agent call the one agent
/// called that, as `agent.get` finds it.
fn named(state: &State, id: &str, agents: bool) -> Result<u32, Refused> {
    if let Some(terminal) = terminal_of(id) {
        return Ok(terminal);
    }
    if agents {
        let called: Vec<u32> = state
            .now
            .rows
            .iter()
            .filter(|r| r.agent && r.name.as_deref() == Some(id))
            .map(|r| r.terminal)
            .collect();
        if let [one] = called.as_slice() {
            return Ok(*one);
        }
        return Err(not_found("agent", id));
    }
    Err(not_found("pane", id))
}

impl Host {
    /// The terminal a call is about: the pane it names; else the pane it comes from
    /// (`caller_pane_id`, from the shim's `HERDR_PANE_ID`); else, once the interface has said
    /// what it shows, that pane. A popup a plugin opened stands for the pane before it. With
    /// none of these the call is refused, never sent to a guess.
    fn target(&self, params: &Value, key: &str) -> Result<u32, Refused> {
        let state = self.state.lock().unwrap();
        let wanted = match param(params, key) {
            Some(id) => Some(named(&state, id, key == "target")?),
            None => match param(params, "caller_pane_id").and_then(terminal_of) {
                Some(caller) => default_target(Some(caller), &state.popups),
                None if state.reported => default_target(state.focused, &state.popups),
                None => None,
            },
        };
        let wanted = wanted.ok_or((
            "pane_not_found",
            "no pane named, none called from, and the interface has not said what it shows"
                .to_string(),
        ))?;
        if !state.now.rows.iter().any(|r| r.terminal == wanted) {
            return Err(not_found("pane", &wanted.to_string()));
        }
        Ok(wanted)
    }

    /// The leaves of the tab `params[key]` names.
    fn tab_leaves(&self, params: &Value, key: &str) -> Result<Vec<u32>, Refused> {
        let id = param(params, key).unwrap_or_default();
        self.state
            .lock()
            .unwrap()
            .now
            .tabs
            .iter()
            .find(|t| t.id() == id)
            .map(|t| t.leaves.clone())
            .ok_or_else(|| not_found("tab", id))
    }

    /// The space `params[key]` names.
    fn space(&self, params: &Value, key: &str) -> Result<i64, Refused> {
        let id = param(params, key).unwrap_or_default();
        let known = |space: i64| {
            self.state
                .lock()
                .unwrap()
                .now
                .spaces
                .iter()
                .any(|s| s.id == space)
        };
        match Id::parse(id) {
            Some(Id::Workspace(space)) if known(space) => Ok(space),
            _ => Err(not_found("workspace", id)),
        }
    }

    /// Where a new tab goes when a call names no space: its pane's, else the first.
    fn some_space(&self, params: &Value) -> Result<i64, Refused> {
        if let Ok(pane) = self.target(params, "")
            && let Some(space) = self.space_of(pane)
        {
            return Ok(space);
        }
        let state = self.state.lock().unwrap();
        state
            .now
            .spaces
            .first()
            .map(|s| s.id)
            .ok_or(("workspace_not_found", "there is no space".to_string()))
    }

    fn space_of(&self, terminal: u32) -> Option<i64> {
        self.state
            .lock()
            .unwrap()
            .now
            .rows
            .iter()
            .find(|r| r.terminal == terminal)
            .map(|r| r.workspace)
    }

    fn pane_id(&self, terminal: u32) -> String {
        Id::Pane {
            workspace: self.space_of(terminal).unwrap_or_default(),
            terminal,
        }
        .to_string()
    }

    /// Refused when a call names a space and a target pane in another one.
    fn same_space(&self, params: &Value, target: u32) -> Result<(), Refused> {
        if param(params, "workspace_id").is_some() {
            let space = self.space(params, "workspace_id")?;
            if self.space_of(target) != Some(space) {
                return Err((
                    "invalid_request",
                    "invalid request: the target pane is in another space".into(),
                ));
            }
        }
        Ok(())
    }

    /// Show `terminal` in every interface, and take it as the focus from now.
    async fn focus(&self, terminal: u32) -> Result<(), Refused> {
        control(ControlRequest::Focus {
            id: crate::request_id("plugin-host"),
            terminal,
        })
        .await?;
        self.set_focus(terminal, false);
        Ok(())
    }

    /// Put `new` beside `target`, in the direction herdr names (`right` or `down`), in the
    /// split tree as the daemon has it now.
    async fn beside(
        &self,
        target: u32,
        new: u32,
        direction: &str,
        ratio: Option<f64>,
    ) -> Result<(), Refused> {
        self.refresh_now().await?;
        let dir = if direction == "down" {
            SplitDir::H
        } else {
            SplitDir::V
        };
        let permille = ratio.map_or(500, |r| (r * 1000.0).clamp(50.0, 950.0) as u16);
        let split = Pane::Split {
            dir,
            permille,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: target }),
            second: Box::new(Pane::Leaf { terminal: new }),
        };
        let tree = self
            .state
            .lock()
            .unwrap()
            .now
            .tabs
            .iter()
            .find(|t| t.leaves.contains(&target))
            .and_then(|t| t.tree.clone());
        let tree = match tree {
            Some(tree) => replace_leaf(&tree, target, &split),
            None => split,
        };
        control(ControlRequest::SetSplit {
            id: crate::request_id("plugin-host"),
            tree,
        })
        .await
        .map(drop)
    }

    /// End terminals; a plugin popup among them gives focus back to the pane before it.
    /// Answered once the daemon no longer lists them, as `qoop close` waits, so a plugin that
    /// lists right after a close never finds what it closed.
    async fn close(&self, terminals: &[u32]) -> Result<(), Refused> {
        for terminal in terminals {
            // Its going is told as `pane_closed`, a close made through the API.
            self.state.lock().unwrap().closing.insert(*terminal);
            let closed = control(ControlRequest::CloseTerminal {
                id: crate::request_id("plugin-host"),
                terminal: *terminal,
            })
            .await;
            if let Err(refused) = closed {
                self.state.lock().unwrap().closing.remove(terminal);
                return Err(refused);
            }
            // Focus goes back only from a popup still shown: one that moved focus elsewhere
            // before closing keeps that.
            let previous = {
                let mut state = self.state.lock().unwrap();
                state.zoomed.remove(terminal);
                let shown = state.focused == Some(*terminal);
                state
                    .popups
                    .remove(terminal)
                    .and_then(|p| p.previous)
                    .filter(|_| shown)
            };
            if let Some(previous) = previous {
                let _ = self.focus(previous).await;
            }
        }
        let until = Instant::now() + CLOSE_PATIENCE;
        while Instant::now() < until {
            self.refresh_now().await?;
            let listed = self
                .state
                .lock()
                .unwrap()
                .now
                .rows
                .iter()
                .any(|r| terminals.contains(&r.terminal));
            if !listed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// A listing after a change, then `read` on it.
    async fn after<R>(&self, read: impl FnOnce(&translate::View) -> R) -> Result<R, Refused> {
        self.refresh_now().await?;
        Ok(self.with_view(read))
    }

    /// A listing now, never one reused from a moment ago: a write was just made.
    async fn refresh_now(&self) -> Result<(), Refused> {
        self.forget_listing();
        self.refresh().await
    }

    fn pane_info(view: &translate::View, terminal: u32) -> Value {
        host::pane_of(view.panes(), terminal).unwrap_or(Value::Null)
    }

    fn tab_info(view: &translate::View, terminal: u32) -> Value {
        let tab = view.tabs.iter().find(|t| t.leaves.contains(&terminal));
        tab.and_then(|t| view.tab_infos().into_iter().find(|i| i["tab_id"] == t.id()))
            .unwrap_or(Value::Null)
    }

    fn workspace_info(view: &translate::View, space: i64) -> Value {
        let id = Id::Workspace(space).to_string();
        view.workspace_infos()
            .into_iter()
            .find(|w| w["workspace_id"] == id)
            .unwrap_or(Value::Null)
    }

    fn agent_info(view: &translate::View, terminal: u32) -> Value {
        let id = terminal.to_string();
        view.agents()
            .into_iter()
            .find(|a| a["terminal_id"].as_str() == Some(id.as_str()))
            .unwrap_or(Value::Null)
    }

    /// A new terminal, its shell started with `env` when there is any. The environment never
    /// passes through the line editor: the shell is handed the same `pane-exec` line a plugin
    /// pane is, whose spec is the shell itself as qoop started it, with `env` added.
    async fn new_terminal(
        &self,
        space: i64,
        cwd: Option<String>,
        name: Option<String>,
        env: Vec<(String, String)>,
    ) -> Result<u32, Refused> {
        let terminal = create(space, cwd.clone(), name).await?;
        if env.is_empty() {
            return Ok(terminal);
        }
        // Whatever fails from here, the terminal made for it is not left behind as a plain shell.
        match self.hand_env(terminal, cwd, env).await {
            Ok(()) => Ok(terminal),
            Err(e) => {
                let _ = self.close(&[terminal]).await;
                Err(e)
            }
        }
    }

    /// Hand a new terminal's shell its `pane-exec` line, whose spec is that shell again as
    /// qoop started it, with `env` added.
    async fn hand_env(
        &self,
        terminal: u32,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    ) -> Result<(), Refused> {
        // Read once the shell has drawn: before that its process may still be the daemon's
        // fork, with no command line of its own yet.
        drawn(terminal).await?;
        self.refresh_now().await?;
        let row = self
            .state
            .lock()
            .unwrap()
            .now
            .rows
            .iter()
            .find(|r| r.terminal == terminal)
            .cloned();
        let pid = row
            .as_ref()
            .and_then(|r| r.shell_pid)
            .and_then(|pid| i32::try_from(pid).ok())
            .map(qoop_pty::Pid);
        let argv = pid
            .map(qoop_pty::sys::argv_of)
            .filter(|argv| !argv.is_empty())
            .ok_or((
                "failed",
                "the new shell's command line cannot be read".to_string(),
            ))?;
        let env = shell_env(|name| pid.and_then(|p| qoop_pty::sys::env_of(p, name)), env);
        let cwd = cwd
            .or_else(|| row.and_then(|r| r.cwd))
            .unwrap_or_else(|| "/".to_string());
        let nonce = nonce().map_err(|e| ("failed", e.to_string()))?;
        self.specs.put_at(
            &nonce,
            Spec {
                argv,
                env,
                cwd: PathBuf::from(cwd),
                scrub: false,
            },
        );
        // Drawn already, above.
        type_line(terminal, &exec_line(&nonce)?).await
    }

    /// qoop's own methods on the host socket, never among herdr's.
    pub(super) async fn internal(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        match method {
            // The interface says which terminal it shows. Only a terminal that exists.
            "qoop.focus" => {
                let terminal = params
                    .get("terminal")
                    .and_then(Value::as_u64)
                    .and_then(|t| u32::try_from(t).ok())
                    .ok_or(("invalid_request", "invalid request: `terminal`".to_string()))?;
                self.refresh().await?;
                if !self.set_focus(terminal, true) {
                    return Err(not_found("pane", &terminal.to_string()));
                }
                Ok(json!({"type": "ok"}))
            }
            // A pane's `pane-exec` fetching what it becomes: once, within its time.
            "qoop.pane_spec" => {
                let nonce = param(params, "nonce").unwrap_or_default();
                let spec = self.specs.take(nonce).ok_or((
                    "spec_not_found",
                    "no pane is waiting under this nonce (used, expired or never made)".to_string(),
                ))?;
                Ok(
                    json!({"type": "pane_spec", "argv": spec.argv, "env": spec.env,
                          "cwd": spec.cwd, "scrub": spec.scrub}),
                )
            }
            _ => Err((
                "invalid_request",
                format!("invalid request: unknown variant `{method}`"),
            )),
        }
    }

    /// Every herdr method that changes something, each in a future of its own.
    pub(super) async fn write(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        self.refresh().await?;
        match method {
            "pane.send_text" => Box::pin(self.send_text(params)).await,
            "pane.send_input" => Box::pin(self.send_input(params)).await,
            "agent.prompt" => Box::pin(self.prompt(params)).await,
            "pane.close" | "plugin.pane.close" => Box::pin(self.close_pane(method, params)).await,
            "popup.close" => Box::pin(self.close_popup(params)).await,
            "pane.focus" | "agent.focus" => Box::pin(self.focus_pane(method, params)).await,
            "pane.rename" | "agent.rename" => Box::pin(self.rename_pane(method, params)).await,
            "pane.zoom" => Box::pin(self.zoom(params)).await,
            "pane.split" => Box::pin(self.split(params)).await,
            "tab.create" => Box::pin(self.create_tab(params)).await,
            "tab.rename" => Box::pin(self.rename_tab(params)).await,
            "tab.focus" => Box::pin(self.focus_tab(params)).await,
            "tab.close" => Box::pin(self.close_tab(params)).await,
            "workspace.create" => Box::pin(self.create_space(params)).await,
            "workspace.focus" => Box::pin(self.focus_space(params)).await,
            "workspace.rename" => Box::pin(self.rename_space(params)).await,
            "workspace.close" => Box::pin(self.close_space(params)).await,
            "notification.show" => Ok(notification(params)),
            "plugin.action.invoke" => self.invoke_param(params),
            "plugin.pane.open" => Box::pin(self.open_pane(params)).await,
            _ => Err((
                "invalid_request",
                format!("invalid request: unknown variant `{method}`"),
            )),
        }
    }

    /// `pane.send_text`: the text as it is, never pasted, as herdr sends it.
    async fn send_text(&self, params: &Value) -> Result<Value, Refused> {
        let terminal = self.target(params, "pane_id")?;
        let text = param(params, "text").unwrap_or_default();
        typed(text, &[])?;
        send(terminal, text, false, Vec::new()).await?;
        Ok(json!({"type": "ok"}))
    }

    /// `pane.send_input` (and `pane run`): the text pasted, then the keys after it.
    async fn send_input(&self, params: &Value) -> Result<Value, Refused> {
        let terminal = self.target(params, "pane_id")?;
        let text = param(params, "text").unwrap_or_default();
        let keys = params
            .get("keys")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let presses = typed(text, &keys)?;
        send(terminal, text, true, presses).await?;
        Ok(json!({"type": "ok"}))
    }

    /// `agent.prompt`, through `qoop prompt`; with `wait`, answered once the agent reaches one
    /// of `until` (idle, done or blocked when not said) after it was seen at work, as herdr
    /// waits.
    async fn prompt(&self, params: &Value) -> Result<Value, Refused> {
        let terminal = self.target(params, "target")?;
        let text = param(params, "text").unwrap_or_default();
        if text.is_empty() {
            return Err(("empty_agent_prompt", "agent prompt text is empty".into()));
        }
        typed(text, &[])?;
        qoop(vec![
            "prompt".into(),
            terminal.to_string(),
            "--".into(),
            text.into(),
        ])
        .await?;
        if let Some(wait) = params.get("wait").filter(|w| w.is_object()) {
            Box::pin(self.wait_for_turn(terminal, wait)).await?;
        }
        let agent = self.after(|v| Self::agent_info(v, terminal)).await?;
        Ok(json!({"type": "agent_prompted", "agent": agent}))
    }

    async fn wait_for_turn(&self, terminal: u32, wait: &Value) -> Result<(), Refused> {
        let until: Vec<String> = wait["until"]
            .as_array()
            .map(|u| {
                u.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|u: &Vec<String>| !u.is_empty())
            .unwrap_or_else(|| vec!["idle".into(), "done".into(), "blocked".into()]);
        // ponytail: herdr waits without end; the host stops at WAIT_CAP so a peer that never
        // leaves cannot hold a task for ever (`serve` drops the wait when the peer leaves).
        let started = Instant::now();
        let deadline = started
            + wait["timeout_ms"]
                .as_u64()
                .map_or(WAIT_CAP, |ms| Duration::from_millis(ms).min(WAIT_CAP));
        let mut at_work = false;
        loop {
            // The follow loop keeps the state current (each agent change wakes it).
            let status = {
                let state = self.state.lock().unwrap();
                let row = state.now.rows.iter().find(|r| r.terminal == terminal);
                row.map(translate::agent_status)
            }
            .ok_or((
                "agent_not_running",
                "the agent's pane went away".to_string(),
            ))?;
            at_work |= matches!(status, "working" | "blocked");
            if at_work && until.iter().any(|u| u == status) {
                return Ok(());
            }
            if !at_work && started.elapsed() > TURN_START {
                return Err((
                    "agent_prompt_stalled",
                    format!(
                        "agent prompt produced no observed working or blocked state within \
                         {} ms; current status is {status}",
                        TURN_START.as_millis()
                    ),
                ));
            }
            if Instant::now() >= deadline {
                return Err(("timeout", "timed out waiting for agent status".into()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn close_pane(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        let terminal = self.target(params, "pane_id")?;
        let id = self.pane_id(terminal);
        self.close(&[terminal]).await?;
        Ok(if method == "pane.close" {
            json!({"type": "ok"})
        } else {
            json!({"type": "plugin_pane_closed", "pane_id": id})
        })
    }

    /// The calling plugin's newest popup (`caller_plugin_id`, from the shim's
    /// `HERDR_PLUGIN_ID`), else the popup it is called from, else the one shown, else the
    /// newest of all.
    async fn close_popup(&self, params: &Value) -> Result<Value, Refused> {
        let popup = {
            let state = self.state.lock().unwrap();
            // Popups only: an overlay is closed as a pane, never by `popup.close`.
            let popups: BTreeMap<u32, &Popup> = state
                .popups
                .iter()
                .filter(|(_, p)| p.popup)
                .map(|(t, p)| (*t, p))
                .collect();
            let newest_of = |plugin: &str| {
                popups
                    .iter()
                    .filter(|(_, p)| p.plugin == plugin)
                    .map(|(t, _)| *t)
                    .next_back()
            };
            let caller = param(params, "caller_pane_id")
                .and_then(terminal_of)
                .filter(|t| popups.contains_key(t));
            param(params, "caller_plugin_id")
                .and_then(newest_of)
                .or(caller)
                .or(state.focused.filter(|t| popups.contains_key(t)))
                .or_else(|| popups.keys().next_back().copied())
        };
        let Some(popup) = popup else {
            return Err(("popup_not_open", "no popup is open".into()));
        };
        self.close(&[popup]).await?;
        Ok(json!({"type": "ok"}))
    }

    async fn focus_pane(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        let key = if method == "pane.focus" {
            "pane_id"
        } else {
            "target"
        };
        let terminal = self.target(params, key)?;
        self.focus(terminal).await?;
        if method == "pane.focus" {
            let pane = self.after(|v| Self::pane_info(v, terminal)).await?;
            Ok(json!({"type": "pane_info", "pane": pane}))
        } else {
            let agent = self.after(|v| Self::agent_info(v, terminal)).await?;
            Ok(json!({"type": "agent_info", "agent": agent}))
        }
    }

    /// `pane rename` and `agent rename` both name the terminal, and so does `tab rename` for a
    /// tab's first pane: qoop has one name per terminal, which is the tab's label too, where
    /// herdr keeps three.
    async fn rename_pane(&self, method: &str, params: &Value) -> Result<Value, Refused> {
        let (key, label) = if method == "pane.rename" {
            ("pane_id", "label")
        } else {
            ("target", "name")
        };
        let terminal = self.target(params, key)?;
        let name = param(params, label).filter(|n| !n.is_empty());
        control(ControlRequest::RenameTerminal {
            id: crate::request_id("plugin-host"),
            terminal,
            name: name.map(str::to_string),
        })
        .await?;
        self.state.lock().unwrap().renamed.insert(terminal);
        if method == "pane.rename" {
            let pane = self.after(|v| Self::pane_info(v, terminal)).await?;
            Ok(json!({"type": "pane_info", "pane": pane}))
        } else {
            let agent = self.after(|v| Self::agent_info(v, terminal)).await?;
            Ok(json!({"type": "agent_info", "agent": agent}))
        }
    }

    /// Zoom is the interface's own in qoop: the host keeps what herdr answers, and `on` focuses
    /// the pane for real.
    async fn zoom(&self, params: &Value) -> Result<Value, Refused> {
        let terminal = self.target(params, "pane_id")?;
        let mode = param(params, "mode").unwrap_or("toggle");
        let (was, now, focus_changed) = {
            let mut state = self.state.lock().unwrap();
            let was = state.zoomed.contains(&terminal);
            let now = match mode {
                "on" => true,
                "off" => false,
                _ => !was,
            };
            if now {
                state.zoomed.insert(terminal);
            } else {
                state.zoomed.remove(&terminal);
            }
            (was, now, now && state.focused != Some(terminal))
        };
        if now {
            self.focus(terminal).await?;
        }
        let layout = self
            .after(|v| {
                v.tabs
                    .iter()
                    .find(|t| t.leaves.contains(&terminal))
                    .map(|t| translate::layout(t, host::AREA, v.focused, now))
            })
            .await?
            .unwrap_or(Value::Null);
        // A zoom or a focus that changed is a layout that changed, as herdr tells it.
        if was != now || focus_changed {
            let event = {
                let state = self.state.lock().unwrap();
                let tab = state.now.tabs.iter().find(|t| t.leaves.contains(&terminal));
                tab.map(|t| state.layout_event(t))
            };
            if let Some(event) = event {
                let _ = self.events.send(Arc::new(event));
            }
        }
        Ok(json!({"type": "pane_zoom", "zoom": {
            "changed": was != now || focus_changed, "zoom_changed": was != now,
            "focus_changed": focus_changed, "pane_id": self.pane_id(terminal),
            "focused_pane_id": layout["focused_pane_id"].clone(), "zoomed": now,
            "layout": layout,
        }}))
    }

    async fn split(&self, params: &Value) -> Result<Value, Refused> {
        let target = self.target(params, "target_pane_id")?;
        self.same_space(params, target)?;
        let space = self
            .space_of(target)
            .ok_or_else(|| not_found("pane", "target"))?;
        let cwd = param(params, "cwd").map(str::to_string);
        let new = self
            .new_terminal(space, cwd, None, env_param(params)?)
            .await?;
        let direction = param(params, "direction").unwrap_or("right");
        let ratio = params.get("ratio").and_then(Value::as_f64);
        self.beside(target, new, direction, ratio).await?;
        if bool_param(params, "focus", false) {
            self.focus(new).await?;
        }
        let pane = self.after(|v| Self::pane_info(v, new)).await?;
        Ok(json!({"type": "pane_info", "pane": pane}))
    }

    async fn create_tab(&self, params: &Value) -> Result<Value, Refused> {
        let space = match param(params, "workspace_id") {
            Some(_) => self.space(params, "workspace_id")?,
            None => self.some_space(params)?,
        };
        let label = param(params, "label").map(str::to_string);
        let cwd = param(params, "cwd").map(str::to_string);
        let new = self
            .new_terminal(space, cwd, label, env_param(params)?)
            .await?;
        if bool_param(params, "focus", false) {
            self.focus(new).await?;
        }
        let (tab, pane) = self
            .after(|v| (Self::tab_info(v, new), Self::pane_info(v, new)))
            .await?;
        Ok(json!({"type": "tab_created", "tab": tab, "root_pane": pane}))
    }

    /// A tab's label is its first pane's name (see [`Host::rename_pane`]).
    async fn rename_tab(&self, params: &Value) -> Result<Value, Refused> {
        let leaves = self.tab_leaves(params, "tab_id")?;
        control(ControlRequest::RenameTerminal {
            id: crate::request_id("plugin-host"),
            terminal: leaves[0],
            name: param(params, "label")
                .filter(|n| !n.is_empty())
                .map(str::to_string),
        })
        .await?;
        let tab = self.after(|v| Self::tab_info(v, leaves[0])).await?;
        Ok(json!({"type": "tab_info", "tab": tab}))
    }

    async fn focus_tab(&self, params: &Value) -> Result<Value, Refused> {
        let leaves = self.tab_leaves(params, "tab_id")?;
        let focused = self.state.lock().unwrap().focused;
        let shown = focused.filter(|t| leaves.contains(t)).unwrap_or(leaves[0]);
        self.focus(shown).await?;
        let tab = self.after(|v| Self::tab_info(v, shown)).await?;
        Ok(json!({"type": "tab_info", "tab": tab}))
    }

    async fn close_tab(&self, params: &Value) -> Result<Value, Refused> {
        let leaves = self.tab_leaves(params, "tab_id")?;
        self.close(&leaves).await?;
        Ok(json!({"type": "ok"}))
    }

    async fn create_space(&self, params: &Value) -> Result<Value, Refused> {
        let cwd = param(params, "cwd").map(str::to_string);
        let name = param(params, "label")
            .map(str::to_string)
            .or_else(|| {
                cwd.as_deref()
                    .and_then(|c| std::path::Path::new(c).file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "workspace".to_string());
        let made = control(ControlRequest::CreateWorkspace {
            id: crate::request_id("plugin-host"),
            name,
        })
        .await?;
        let ControlResponse::WorkspaceCreated { workspace, .. } = made else {
            return Err(("failed", "unexpected answer to a new space".into()));
        };
        let new = self
            .new_terminal(workspace, cwd, None, env_param(params)?)
            .await?;
        if bool_param(params, "focus", false) {
            self.focus(new).await?;
        }
        let (space, tab, pane) = self
            .after(|v| {
                (
                    Self::workspace_info(v, workspace),
                    Self::tab_info(v, new),
                    Self::pane_info(v, new),
                )
            })
            .await?;
        Ok(
            json!({"type": "workspace_created", "workspace": space, "tab": tab,
                  "root_pane": pane}),
        )
    }

    async fn focus_space(&self, params: &Value) -> Result<Value, Refused> {
        let space = self.space(params, "workspace_id")?;
        let shown = {
            let state = self.state.lock().unwrap();
            let in_space = |t: &u32| {
                state
                    .now
                    .rows
                    .iter()
                    .any(|r| r.terminal == *t && r.workspace == space)
            };
            state.focused.filter(in_space).or_else(|| {
                state
                    .now
                    .rows
                    .iter()
                    .find(|r| r.workspace == space)
                    .map(|r| r.terminal)
            })
        };
        if let Some(terminal) = shown {
            self.focus(terminal).await?;
        }
        let info = self.after(|v| Self::workspace_info(v, space)).await?;
        Ok(json!({"type": "workspace_info", "workspace": info}))
    }

    /// A space always has a name in qoop, so taking it away is refused.
    async fn rename_space(&self, params: &Value) -> Result<Value, Refused> {
        let space = self.space(params, "workspace_id")?;
        let name = param(params, "label")
            .filter(|n| !n.trim().is_empty())
            .ok_or((
                "invalid_request",
                "invalid request: a space keeps a name in qoop".to_string(),
            ))?;
        control(ControlRequest::RenameWorkspace {
            id: crate::request_id("plugin-host"),
            workspace: space,
            name: name.to_string(),
        })
        .await?;
        let info = self.after(|v| Self::workspace_info(v, space)).await?;
        Ok(json!({"type": "workspace_info", "workspace": info}))
    }

    /// herdr closes a space with what is in it; qoop closes only an empty one, so its
    /// terminals end first.
    async fn close_space(&self, params: &Value) -> Result<Value, Refused> {
        let space = self.space(params, "workspace_id")?;
        let terminals: Vec<u32> = self
            .state
            .lock()
            .unwrap()
            .now
            .rows
            .iter()
            .filter(|r| r.workspace == space)
            .map(|r| r.terminal)
            .collect();
        self.close(&terminals).await?;
        control(ControlRequest::CloseWorkspace {
            id: crate::request_id("plugin-host"),
            workspace: space,
        })
        .await?;
        self.refresh_now().await?;
        Ok(json!({"type": "ok"}))
    }

    /// `plugin.action.invoke`: with a `plugin_id`, its action; without, the one action across
    /// every plugin here whose id (or `plugin.action`) is the one asked, refused when more
    /// than one is.
    pub(super) fn invoke_param(&self, params: &Value) -> Result<Value, Refused> {
        let asked = param(params, "action_id").unwrap_or_default().trim();
        let (plugin, action) = match param(params, "plugin_id") {
            Some(plugin) => {
                let plugin = plugin.trim().to_string();
                let found = install::admitted(&self.home, &plugin)
                    .map_err(|_| ("plugin_not_found", "plugin not found".to_string()))?;
                if !found.manifest.actions.iter().any(|a| a.id == asked) {
                    return Err(("plugin_action_not_found", "plugin action not found".into()));
                }
                (plugin, asked.to_string())
            }
            None => {
                let matches: Vec<(String, String)> = install::installed(&self.home)
                    .into_iter()
                    .filter_map(|id| install::admitted(&self.home, &id).ok().map(|a| (id, a)))
                    .flat_map(|(id, a)| {
                        a.manifest
                            .actions
                            .into_iter()
                            .filter(|action| {
                                action.id == asked || format!("{id}.{}", action.id) == asked
                            })
                            .map(|action| (id.clone(), action.id))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                match matches.as_slice() {
                    [one] => one.clone(),
                    [] => {
                        return Err(("plugin_action_not_found", "plugin action not found".into()));
                    }
                    _ => {
                        return Err((
                            "ambiguous_plugin_action",
                            "plugin action id matches more than one action; include plugin_id"
                                .into(),
                        ));
                    }
                }
            }
        };
        self.invoke(&plugin, &action, params.get("context"))
    }

    /// Run one of a plugin's actions. Its context is the focused pane's, with every field a
    /// caller passes put over it one by one; the ids it is told come from that context.
    pub(super) fn invoke(
        &self,
        plugin_id: &str,
        action_id: &str,
        context: Option<&Value>,
    ) -> Result<Value, Refused> {
        let plugin = install::admitted(&self.home, plugin_id)
            .map_err(|_| ("plugin_not_found", "plugin not found".to_string()))?;
        let action = plugin
            .manifest
            .here()
            .actions
            .into_iter()
            .find(|a| a.id == action_id)
            .ok_or_else(|| {
                let os = std::env::consts::OS;
                if plugin.manifest.actions.iter().any(|a| a.id == action_id) {
                    (
                        "platform_unsupported",
                        format!(
                            "action '{plugin_id}.{action_id}' does not support the current platform ({os})"
                        ),
                    )
                } else {
                    ("plugin_action_not_found", "plugin action not found".into())
                }
            })?;
        let mut merged = host::focused_scope(&self.state.lock().unwrap()).context;
        merge_context(&mut merged, context);
        let merged = host::invoked(&merged, "api", "plugin.action.invoke");
        let mut env = install::plugin_env(&self.home, plugin_id, &plugin.root);
        env.extend(host::env_of_context(&merged));
        env.push(("HERDR_PLUGIN_ACTION_ID".into(), action_id.into()));
        env.push((
            "HERDR_PLUGIN_CONTEXT_JSON".into(),
            merged.to_string().into(),
        ));
        let log = self.home.state_of(plugin_id).join("host.log");
        let started_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let command = action.command.clone();
        let mut child = host::spawn(&plugin.root, &command, env, &log)
            .map_err(|e| ("failed", e.to_string()))?;
        std::thread::spawn(move || child.wait());
        let log_id = format!(
            "plugin-log-{}",
            self.log_ids.fetch_add(1, Ordering::Relaxed) + 1
        );
        Ok(json!({"type": "plugin_action_invoked",
                  "action": translate::compact(json!({
                      "plugin_id": plugin_id, "action_id": action.id,
                      "title": action.title.unwrap_or_else(|| action_id.to_string()),
                      "contexts": (!action.contexts.is_empty()).then_some(action.contexts),
                      "command": command,
                      "platforms": action.platforms.or(plugin.manifest.platforms),
                  })),
                  "context": merged,
                  "log": {"log_id": log_id, "plugin_id": plugin_id, "action_id": action_id,
                          "command": command, "status": "running",
                          "started_unix_ms": started_unix_ms}}))
    }

    /// `plugin pane open`, checked as herdr checks it, then a terminal placed as asked whose
    /// shell is handed the constant `exec '<qoop>' plugin pane-exec <nonce>`; the plugin's
    /// argv, env and cwd stay in the host's memory under the nonce and never pass through a
    /// line editor.
    pub(super) async fn open_pane(&self, params: &Value) -> Result<Value, Refused> {
        let plugin_id = param(params, "plugin_id").unwrap_or_default();
        let entrypoint = param(params, "entrypoint").unwrap_or_default();
        let plugin = install::admitted(&self.home, plugin_id)
            .map_err(|_| ("plugin_not_found", "plugin not found".to_string()))?;
        let entry = plugin
            .manifest
            .panes
            .iter()
            .find(|p| p.id == entrypoint)
            .cloned()
            .ok_or_else(|| {
                (
                    "plugin_pane_not_found",
                    format!("plugin pane entrypoint '{entrypoint}' not found"),
                )
            })?;
        if !plugin
            .manifest
            .here()
            .panes
            .iter()
            .any(|p| p.id == entrypoint)
        {
            return Err((
                "platform_unsupported",
                format!(
                    "plugin pane does not support the current platform ({})",
                    std::env::consts::OS
                ),
            ));
        }
        let placement = param(params, "placement")
            .map(str::to_string)
            .or(entry.placement.clone())
            .unwrap_or_else(|| "overlay".to_string());
        let has = |key: &str| params.get(key).is_some_and(|v| !v.is_null());
        let invalid = |message: &str| Err(("invalid_params", message.to_string()));
        if placement != "popup" && (has("width") || has("height")) {
            return invalid("width and height are only supported when placement is popup");
        }
        if placement == "popup" && self.state.lock().unwrap().popups.values().any(|p| p.popup) {
            return Err(("ui_busy", "a popup pane is already open".into()));
        }
        match placement.as_str() {
            "overlay" | "popup"
                if has("workspace_id") || has("target_pane_id") || has("direction") =>
            {
                return invalid("overlay and popup plugin panes target the active pane");
            }
            "split" | "zoomed" if has("workspace_id") => {
                return invalid(
                    "split and zoomed plugin panes target an existing pane; use target_pane_id",
                );
            }
            "tab" if has("target_pane_id") || has("direction") => {
                return invalid(
                    "tab plugin panes support workspace_id but not target_pane_id or direction",
                );
            }
            _ => {}
        }
        let asked = env_param(params)?;

        // The pane it is placed against: the one named, else the active one.
        let target = match (placement.as_str(), param(params, "target_pane_id")) {
            (_, Some(_)) => Some(self.target(params, "target_pane_id")?),
            ("tab", None) => None,
            (_, None) => Some(
                self.target(params, "")
                    .map_err(|_| ("no_active_pane", "no active pane".to_string()))?,
            ),
        };
        let space = match (param(params, "workspace_id"), target) {
            (Some(_), _) => self.space(params, "workspace_id")?,
            (None, Some(t)) => self
                .space_of(t)
                .ok_or_else(|| not_found("pane", &t.to_string()))?,
            (None, None) => self
                .some_space(params)
                .map_err(|_| ("no_active_workspace", "no active workspace".to_string()))?,
        };
        let cwd = param(params, "cwd")
            .map(PathBuf::from)
            .unwrap_or_else(|| plugin.root.clone());

        // Its context: the target pane's for a pane beside one, the space's for a tab.
        let scope = {
            let state = self.state.lock().unwrap();
            match target {
                Some(t) => host::scope(&state.now, space, Some(t)),
                None => host::scope(&state.now, space, None),
            }
        };
        let target_tab = target.and_then(|t| {
            let state = self.state.lock().unwrap();
            state
                .now
                .tabs
                .iter()
                .find(|tab| tab.leaves.contains(&t))
                .map(|tab| tab.id())
        });
        let mut env: Vec<(String, String)> =
            install::plugin_env(&self.home, plugin_id, &plugin.root)
                .into_iter()
                .map(|(k, v)| (k, v.to_string_lossy().into_owned()))
                .collect();
        env.push(("HERDR_PLUGIN_ENTRYPOINT_ID".into(), entrypoint.into()));
        env.push((
            "HERDR_PLUGIN_CONTEXT_JSON".into(),
            scope.context.to_string(),
        ));
        env.extend(asked);
        let argv: Vec<String> = std::iter::once(
            install::program(&plugin.root, &entry.command[0])
                .to_string_lossy()
                .into_owned(),
        )
        .chain(entry.command[1..].iter().cloned())
        .collect();

        let nonce = nonce().map_err(|e| ("failed", e.to_string()))?;
        let opts = crate::supervise::NewOpts {
            name: entry.title.clone(),
            cwd: Some(plugin.root.clone()),
            space: Some(space.to_string()),
            typed: Some(exec_line(&nonce)?),
            ..Default::default()
        };
        let specs = self.specs.clone();
        let tab_placed = placement == "tab";
        let made = tokio::task::spawn_blocking(move || {
            let mut opened = None;
            let result = crate::supervise::open_terminal(&opts, |terminal| {
                // The pane's own ids, after what the caller asked, so they cannot be set
                // from outside: its space, its tab (a new one for a tab, the target's
                // otherwise) and itself.
                let mut env = env;
                let tab = if tab_placed {
                    Some(
                        Id::Tab {
                            workspace: space,
                            key: terminal,
                        }
                        .to_string(),
                    )
                } else {
                    target_tab
                };
                env.push((
                    "HERDR_WORKSPACE_ID".into(),
                    Id::Workspace(space).to_string(),
                ));
                if let Some(tab) = tab {
                    env.push(("HERDR_TAB_ID".into(), tab));
                }
                let pane = Id::Pane {
                    workspace: space,
                    terminal,
                };
                env.push(("HERDR_PANE_ID".into(), pane.to_string()));
                specs.put_at(
                    &nonce,
                    Spec {
                        argv,
                        env,
                        cwd,
                        scrub: true,
                    },
                );
                opened = Some(terminal);
            });
            (result, opened)
        })
        .await
        .map_err(|e| ("failed", e.to_string()))?;
        let new = match made {
            (Ok(()), Some(terminal)) => terminal,
            (Err(e), opened) => {
                // A terminal made but never handed its line is ended, not left as a shell.
                if let Some(stray) = opened {
                    let _ = self.close(&[stray]).await;
                }
                return Err(("plugin_pane_open_failed", e.to_string()));
            }
            (Ok(()), None) => {
                return Err(("plugin_pane_open_failed", "no terminal was opened".into()));
            }
        };
        if !tab_placed && let Some(target) = target {
            let direction = param(params, "direction").unwrap_or("right");
            self.beside(target, new, direction, None).await?;
        }
        if matches!(placement.as_str(), "zoomed" | "popup" | "overlay") {
            let mut state = self.state.lock().unwrap();
            state.zoomed.insert(new);
            if placement != "zoomed" {
                state.popups.insert(
                    new,
                    Popup {
                        previous: target,
                        plugin: plugin_id.to_string(),
                        popup: placement == "popup",
                    },
                );
            }
        }
        // An overlay or a zoomed pane always takes the focus, as in herdr; split and tab only
        // when asked; a popup is in front of everything as the interface shows it.
        let focus = matches!(placement.as_str(), "overlay" | "zoomed" | "popup")
            || bool_param(params, "focus", false);
        if focus {
            self.focus(new).await?;
        }
        let pane = self.after(|v| Self::pane_info(v, new)).await?;
        if placement == "popup" {
            return Ok(json!({"type": "ok"}));
        }
        Ok(json!({"type": "plugin_pane_opened", "plugin_pane": {
            "plugin_id": plugin_id, "entrypoint": entrypoint, "pane": pane,
        }}))
    }
}

/// herdr's `notification.show`: a desktop note as the interface shows one, unless notes are
/// muted on this Mac. The host has no terminal of its own, so only a system program shows it;
/// herdr's `sound` is not played.
fn notification(params: &Value) -> Value {
    let title = param(params, "title").unwrap_or_default();
    let text = match param(params, "body").filter(|b| !b.is_empty()) {
        Some(body) => format!("{title}: {body}"),
        None => title.to_string(),
    };
    let (allowed, reason) = may_show(qoop_runtime::config::mute_in(&qoop_runtime::config::path()));
    let (shown, reason) = if !allowed {
        (false, reason)
    } else {
        let mut env = qoop_tui::notify::Env::read();
        env.term_program = None;
        env.term = None;
        env.kitty = false;
        env.tmux = false;
        match qoop_tui::notify::route(&env) {
            qoop_tui::notify::Route::System(program) => {
                let body = qoop_tui::notify::clean(&text);
                qoop_tui::notify::spawn(&qoop_tui::notify::call(program, &body, None));
                (true, "shown")
            }
            _ => (false, "no_foreground_client"),
        }
    };
    json!({"type": "notification_show", "shown": shown, "reason": reason})
}

/// `qoop plugin pane-exec <nonce>`: fetch this pane's spec from this session's host once
/// (never a socket named in the environment), and become what it says.
pub(crate) fn pane_exec(nonce: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let answer = super::shim::ask(
        &host::socket_path(),
        "qoop.pane_spec",
        json!({"nonce": nonce}),
    )?;
    if let Some(error) = answer.get("error") {
        return Err(std::io::Error::other(format!(
            "this pane cannot start: {}",
            error["message"].as_str().unwrap_or_default()
        )));
    }
    let spec = &answer["result"];
    let argv: Vec<String> = spec["argv"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let Some(program) = argv.first() else {
        return Err(std::io::Error::other("this pane has nothing to run"));
    };
    let env: Vec<(String, String)> = spec["env"]
        .as_array()
        .map(|pairs| {
            pairs
                .iter()
                .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let cwd = spec["cwd"].as_str().unwrap_or("/");
    let scrub = spec["scrub"].as_bool().unwrap_or(true);
    let mut cmd = exec_command(program, &argv[1..], &env, cwd, scrub);
    Err(cmd.exec())
}

/// The command `pane-exec` becomes, in its directory with the spec's variables set. For a
/// plugin's program, qoop's own variables and a parent agent's are gone first.
pub(crate) fn exec_command(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: &str,
    scrub: bool,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).current_dir(cwd);
    if scrub {
        install::scrub(&mut cmd);
    }
    for (name, value) in env {
        if env_name(name) {
            cmd.env(name, value);
        }
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            argv: vec!["bin/x".into()],
            env: vec![("A".into(), "1".into())],
            cwd: PathBuf::from("/tmp"),
            scrub: true,
        }
    }

    #[test]
    fn a_spec_is_taken_once_and_not_after_its_time() {
        let specs = Specs::default();
        let first = nonce().expect("nonce");
        assert_eq!(first.len(), 32, "128 bits as hex");
        specs.put_at(&first, spec());
        assert_eq!(specs.take(&first), Some(spec()));
        assert_eq!(specs.take(&first), None, "used");
        assert_eq!(specs.take("not-a-nonce"), None);
        specs.0.lock().unwrap().insert(
            "old".into(),
            (Instant::now() - SPEC_TTL - Duration::from_secs(1), spec()),
        );
        assert_eq!(specs.take("old"), None, "expired");
        assert_ne!(nonce().expect("nonce"), first, "a fresh nonce each time");
    }

    fn envs(cmd: &std::process::Command) -> BTreeMap<String, Option<String>> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn pane_exec_leaves_qoop_behind_for_a_plugin_and_sets_only_valid_names() {
        let env = vec![
            ("HERDR_PANE_ID".to_string(), "w1:p3".to_string()),
            ("QOOP_TOKEN".to_string(), "from the spec".to_string()),
            ("bad name".to_string(), "y".to_string()),
        ];
        let plugin = exec_command("/bin/true", &[], &env, "/tmp", true);
        let plugin_env = envs(&exec_command("/bin/true", &[], &env, "/tmp", true));
        assert_eq!(plugin_env["HERDR_PANE_ID"], Some("w1:p3".into()));
        assert!(!plugin_env.contains_key("bad name"));
        assert_eq!(plugin.get_current_dir(), Some(std::path::Path::new("/tmp")));
        let shell_env = envs(&exec_command("/bin/sh", &[], &env, "/tmp", false));
        assert!(
            !shell_env.values().any(Option::is_none),
            "a shell keeps its own variables: nothing removed"
        );
        assert!(env_name("_A1") && !env_name("1A") && !env_name("A-B") && !env_name(""));
    }

    #[test]
    fn herdr_keys_press_what_herdr_sends() {
        let bytes = |k: &str| match press(k).expect(k) {
            Press::Bytes(b) => b,
            other => panic!("{k}: {other:?}"),
        };
        assert_eq!(bytes("Enter"), b"\r");
        assert_eq!(bytes(" enter "), b"\r", "trimmed");
        assert_eq!(bytes("ctrl+u"), [0x15]);
        assert_eq!(bytes("control+u"), [0x15]);
        assert_eq!(bytes("C-c"), [0x03]);
        assert_eq!(bytes("ctrl+shift+a"), [0x01]);
        assert_eq!(bytes("ctrl+["), b"\x1b");
        assert_eq!(bytes("ctrl+\\"), [0x1c]);
        assert_eq!(bytes("Escape"), b"\x1b");
        assert_eq!(bytes("q"), b"q");
        assert_eq!(bytes("Q"), b"Q");
        assert_eq!(bytes("shift+a"), b"A");
        assert_eq!(bytes("space"), b" ");
        assert_eq!(bytes("plus"), b"+");
        assert_eq!(bytes("ctrl++"), b"+");
        assert_eq!(bytes("double-quote"), b"\"");
        assert_eq!(bytes("F1"), b"\x1bOP");
        assert_eq!(bytes("f3"), b"\x1bOR");
        assert_eq!(bytes("shift+f3"), b"\x1b[13;2~");
        assert_eq!(bytes("ctrl+f1"), b"\x1b[1;5P");
        assert_eq!(bytes("f12"), b"\x1b[24~");
        assert_eq!(bytes("f13"), b"\x1b[25~");
        assert_eq!(bytes("ctrl+f5"), b"\x1b[15;5~");
        assert_eq!(bytes("BS"), b"\x7f");
        assert_eq!(bytes("ctrl+backspace"), [0x08]);
        assert_eq!(bytes("alt+x"), b"\x1bx");
        assert_eq!(bytes("option+x"), b"\x1bx");
        assert_eq!(bytes("shift+tab"), b"\x1b[Z");
        assert_eq!(bytes("ctrl+left"), b"\x1b[1;5D");
        assert_eq!(bytes("shift+up"), b"\x1b[1;2A");
        assert_eq!(bytes("alt+up"), b"\x1b[1;3A");
        // A plain arrow goes through `qoop send --key`, which follows the cursor key mode.
        assert_eq!(press("up").expect("up"), Press::Key("up"));
        assert_eq!(press("Left").expect("left"), Press::Key("left"));
        for nothing in ["f26", "hyper+a", "cmd+c", "super+left"] {
            assert_eq!(press(nothing).expect(nothing), Press::Nothing, "{nothing}");
        }
        for bad in ["home", "pageup", "ctrl+space", "ctrl", "a+b", "", "f13x"] {
            let err = press(bad).expect_err(bad);
            assert_eq!(err, ("invalid_key", format!("unsupported key {bad}")));
        }
    }

    #[test]
    fn a_call_without_a_target_skips_a_popup_for_the_pane_before_it() {
        let mut popups = BTreeMap::new();
        let popup = |previous| Popup {
            previous,
            plugin: "p".into(),
            popup: true,
        };
        popups.insert(9, popup(Some(4)));
        assert_eq!(default_target(Some(9), &popups), Some(4));
        assert_eq!(default_target(Some(5), &popups), Some(5));
        assert_eq!(default_target(None, &popups), None);
        popups.insert(10, popup(None));
        assert_eq!(default_target(Some(10), &popups), None);
    }

    #[test]
    fn a_split_replaces_its_target_leaf_in_place() {
        let tree = Pane::Split {
            dir: SplitDir::V,
            permille: 500,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: 1 }),
            second: Box::new(Pane::Leaf { terminal: 2 }),
        };
        let with = Pane::Split {
            dir: SplitDir::H,
            permille: 500,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: 2 }),
            second: Box::new(Pane::Leaf { terminal: 7 }),
        };
        assert_eq!(replace_leaf(&tree, 2, &with).leaves(), [1, 2, 7]);
        assert_eq!(replace_leaf(&tree, 9, &with), tree);
    }

    #[test]
    fn a_muted_mac_shows_no_note() {
        use qoop_runtime::config::Mute;
        assert_eq!(may_show(Mute::Off), (true, "shown"));
        assert_eq!(may_show(Mute::Here), (false, "disabled"));
        assert_eq!(may_show(Mute::Everywhere), (false, "disabled"));
    }

    #[test]
    fn a_restarted_shell_keeps_qoops_zsh_integration_and_no_qoop_name_is_asked() {
        let start = |name: &str| match name {
            "ZDOTDIR" => Some("/q/shell".to_string()),
            "QOOP_ZDOTDIR" => Some("/home/me".to_string()),
            _ => None,
        };
        let env = shell_env(start, vec![("HELLO".into(), "yes".into())]);
        assert_eq!(
            env,
            [
                ("ZDOTDIR".to_string(), "/q/shell".to_string()),
                ("QOOP_ZDOTDIR".to_string(), "/home/me".to_string()),
                ("HELLO".to_string(), "yes".to_string()),
            ]
        );
        assert_eq!(shell_env(|_| None, Vec::new()), []);
        for name in ["QOOP_TOKEN", "QORO_TERMINAL"] {
            let refused = env_param(&json!({"env": {name: "x"}})).expect_err(name);
            assert_eq!(refused.0, "invalid_env");
        }
        for name in ["", "A=B", "A\0"] {
            assert!(env_param(&json!({"env": {name: "x"}})).is_err(), "{name:?}");
        }
        let kept = env_param(&json!({"env": {"OK_NAME": "x", "HERDR_PLUGIN_ID": "evil",
                                             "HERDR_SOCKET_PATH": "/elsewhere"}}));
        assert_eq!(kept, Ok(vec![("OK_NAME".to_string(), "x".to_string())]));
    }

    #[test]
    fn a_callers_null_leaves_our_context_field() {
        let mut ours = json!({"pane_id": "w1:p1", "workspace_id": "w1"});
        merge_context(
            &mut ours,
            Some(&json!({"pane_id": null, "workspace_id": "w2", "extra": 1})),
        );
        assert_eq!(
            ours,
            json!({"pane_id": "w1:p1", "workspace_id": "w2", "extra": 1})
        );
    }

    #[test]
    fn a_refusal_gets_herdrs_code() {
        assert_eq!(refusal_code("no such terminal: 4"), "pane_not_found");
        assert_eq!(
            refusal_code("agent 3 is blocked and requires interactive input"),
            "agent_blocked"
        );
        assert_eq!(
            refusal_code("agent 3 is waiting on a trust prompt: answer it in the terminal first"),
            "agent_not_ready"
        );
        assert_eq!(
            refusal_code("nothing is awake: `qoop` or `qoop wake` starts it"),
            "server_not_running"
        );
        assert_eq!(
            refusal_code("a person is at terminal 1 until 5 (in 2 s): nothing was sent"),
            "input_refused"
        );
    }
}

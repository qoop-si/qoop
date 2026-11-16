//! `herdr`, as a plugin runs it: the qoop binary under the name `herdr` in
//! `<data>/plugins/.bin`, which is first on a plugin's `PATH` and is its `HERDR_BIN_PATH`.
//!
//! It reads herdr's CLI forms as herdr 0.9.5 reads them and asks the plugin host on its socket.
//! Output is herdr's: the envelope on stdout, a refusal's envelope on stderr with exit 1, a
//! command called wrong as plain text on stderr with exit 2; `pane read` prints its text, and
//! the commands herdr runs quietly print nothing. Only the plugins' own `.bin/herdr` is ever
//! dispatched here, so a `herdr` elsewhere on the machine (the person's own) is never shadowed.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::install::Home;
use super::translate;

/// How long the shim waits for the host's answer.
const PATIENCE: Duration = Duration::from_secs(30);

/// Whether this process is the shim: started as `herdr`, from the plugins' `.bin`. A bare
/// `herdr` (found on `PATH`) counts when the first `herdr` on `PATH` is that one.
pub(crate) fn invoked() -> bool {
    let Some(argv0) = std::env::args_os().next().map(PathBuf::from) else {
        return false;
    };
    if argv0.file_name().is_none_or(|n| n != "herdr") {
        return false;
    }
    let Ok(bin) = Home::here().map(|h| h.bin()) else {
        return false;
    };
    let found = if argv0.components().count() > 1 {
        Some(argv0)
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("herdr"))
                .find(|candidate| candidate.exists())
        })
    };
    is_shim(found.as_deref(), &bin)
}

/// Whether `found` is `bin/herdr`, compared by the folder it is in (the link itself is never
/// followed: it points at qoop wherever qoop is).
fn is_shim(found: Option<&Path>, bin: &Path) -> bool {
    let folder = |p: &Path| p.parent().and_then(|d| d.canonicalize().ok());
    found.is_some_and(|f| {
        f.file_name().is_some_and(|n| n == "herdr")
            && folder(f).is_some()
            && folder(f) == bin.canonicalize().ok()
    })
}

/// What a CLI form does.
#[derive(Debug, PartialEq)]
enum Call {
    /// Printed here, asking nobody.
    Print(String),
    /// Asked of the host; its envelope printed.
    Ask(&'static str, Value),
    /// Asked of the host; `result.read.text` printed as it is (`pane read`).
    AskText(&'static str, Value),
    /// Asked of the host; nothing printed on success, as herdr runs these.
    Quiet(&'static str, Value),
}

/// The options that take a value, so a value is never read as a positional.
const VALUED: &[&str] = &[
    "--workspace",
    "--pane",
    "--tab",
    "--source",
    "--lines",
    "--format",
    "--plugin",
    "--direction",
    "--amount",
    "--target-pane",
    "--source-pane",
    "--cwd",
    "--label",
    "--env",
    "--placement",
    "--entrypoint",
    "--width",
    "--height",
    "--ratio",
    "--body",
    "--sound",
    "--position",
    "--title",
    "--token",
    "--clear-token",
    "--agent",
    "--seq",
    "--ttl-ms",
    "--display-agent",
    "--applies-to-source",
    "--state-label",
    "--split",
    "--until",
    "--timeout",
];

/// The words after a command, in order: options with their values, and positionals.
struct Words<'a> {
    plain: Vec<&'a str>,
    options: Vec<(&'a str, Option<&'a str>)>,
}

impl<'a> Words<'a> {
    fn of(words: &'a [String]) -> Self {
        let mut plain = Vec::new();
        let mut options = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let word = words[i].as_str();
            if word == "--" {
                plain.extend(words[i + 1..].iter().map(String::as_str));
                break;
            }
            if let Some((name, value)) = word.split_once('=').filter(|_| word.starts_with("--")) {
                options.push((name, Some(value)));
            } else if VALUED.contains(&word) {
                options.push((word, words.get(i + 1).map(String::as_str)));
                i += 1;
            } else if word.starts_with("--") {
                options.push((word, None));
            } else {
                plain.push(word);
            }
            i += 1;
        }
        Self { plain, options }
    }

    /// The last value given to `name`: a repeated option is the last one, as in herdr.
    fn value(&self, name: &str) -> Option<&'a str> {
        self.options
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| *v)
    }

    fn has(&self, name: &str) -> bool {
        self.options.iter().any(|(n, _)| *n == name)
    }

    /// `--focus` or `--no-focus`, whichever came last.
    fn focus(&self) -> Option<bool> {
        self.options.iter().rev().find_map(|(n, _)| match *n {
            "--focus" => Some(true),
            "--no-focus" => Some(false),
            _ => None,
        })
    }

    /// Every `--env KEY=VALUE`, as an object; a later key wins.
    fn env(&self) -> Result<Value, String> {
        let mut out = serde_json::Map::new();
        for (name, value) in &self.options {
            if *name != "--env" {
                continue;
            }
            let (key, value) = value
                .and_then(|kv| kv.split_once('='))
                .ok_or("env must use KEY=VALUE")?;
            if key.is_empty() {
                return Err("env key must not be empty".into());
            }
            out.insert(key.to_string(), json!(value));
        }
        Ok(Value::Object(out))
    }
}

/// Put `key: value` into `params` when there is a value.
fn with(mut params: Value, key: &str, value: Option<&str>) -> Value {
    if let Some(value) = value {
        params[key] = json!(value);
    }
    params
}

/// The pane a `split` or `zoom` is about: a positional, `--pane`, or `--current`, the later one
/// winning. `--current` is the caller's own pane for a split and no pane at all (the focused
/// one) for a zoom.
fn pane_option(
    raw: &[String],
    caller: Option<&str>,
    current_is_caller: bool,
) -> Result<Option<String>, String> {
    let mut pane = None;
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--pane" => {
                pane = raw.get(i + 1).cloned();
                i += 1;
            }
            "--current" if current_is_caller => {
                pane = Some(
                    caller
                        .ok_or("--current requires HERDR_PANE_ID")?
                        .to_string(),
                );
            }
            "--current" => pane = None,
            word if VALUED.contains(&word) => i += 1,
            word if word.starts_with("--") => {}
            word if pane.is_none() => pane = Some(word.to_string()),
            _ => {}
        }
        i += 1;
    }
    Ok(pane)
}

/// The text of `send-text` and `run`: every word after the pane as written, dashes and a `--`
/// included, as herdr keeps them; an empty argument is empty text.
fn rest_text(raw: &[String]) -> Result<String, String> {
    match raw.split_first() {
        Some((_, rest)) if !rest.is_empty() => Ok(rest.join(" ")),
        _ => Err("missing required text".into()),
    }
}

/// `report-metadata`'s options, as herdr's CLI sends them: `--source` (required), `--token
/// NAME=VALUE` and `--clear-token NAME` (a later one for the same name wins), `--seq`,
/// `--ttl-ms`; a pane's also its presentation fields.
fn metadata(w: &Words, pane: bool) -> Result<Value, String> {
    let source = w
        .value("--source")
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing required --source")?;
    let mut params = json!({"source": source});
    let mut tokens = serde_json::Map::new();
    for (name, value) in &w.options {
        match (*name, value) {
            ("--token", Some(kv)) => {
                let (key, value) = kv.split_once('=').ok_or("token must use NAME=VALUE")?;
                if key.is_empty() {
                    return Err("token name must not be empty".into());
                }
                tokens.insert(key.into(), json!(value));
            }
            ("--clear-token", Some(key)) => {
                tokens.insert((*key).into(), Value::Null);
            }
            _ => {}
        }
    }
    if !tokens.is_empty() {
        params["tokens"] = Value::Object(tokens);
    } else if !pane {
        return Err("missing token to set or clear".into());
    }
    for (flag, key) in [("--seq", "seq"), ("--ttl-ms", "ttl_ms")] {
        if let Some(n) = w.value(flag) {
            let n: u64 = n
                .parse()
                .map_err(|_| format!("invalid value '{n}' for '{flag}'"))?;
            params[key] = json!(n);
        }
    }
    if pane {
        for (flag, key) in [
            ("--title", "title"),
            ("--display-agent", "display_agent"),
            ("--agent", "agent"),
            ("--applies-to-source", "applies_to_source"),
        ] {
            params = with(params, key, w.value(flag));
        }
        for (flag, key) in [
            ("--clear-title", "clear_title"),
            ("--clear-display-agent", "clear_display_agent"),
            ("--clear-state-labels", "clear_state_labels"),
        ] {
            if w.has(flag) {
                params[key] = json!(true);
            }
        }
        let mut labels = serde_json::Map::new();
        for (name, value) in &w.options {
            if *name == "--state-label"
                && let Some((status, label)) = value.and_then(|v| v.split_once('='))
            {
                labels.insert(status.into(), json!(label));
            }
        }
        if !labels.is_empty() {
            params["state_labels"] = Value::Object(labels);
        }
    }
    Ok(params)
}

/// A herdr CLI form, as the host is asked it. `caller` is the plugin's own pane
/// (`HERDR_PANE_ID`); `socket` is where the host is, for `session list`.
fn parse(args: &[String], caller: Option<&str>, socket: &Path) -> Result<Call, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let tail = |n: usize| &args[n.min(args.len())..];
    let rest = |n: usize| Words::of(tail(n));
    let call = match words.as_slice() {
        ["--version" | "-V", ..] => Call::Print(format!("herdr {}\n", translate::VERSION)),
        ["api", "snapshot", ..] => Call::Ask("session.snapshot", json!({})),
        ["session", "list", more @ ..] => {
            let json = match more {
                [] => false,
                ["--json"] => true,
                _ => return Err("usage: herdr session list [--json]".into()),
            };
            Call::Print(session_list(socket, json))
        }
        ["pane", "list", ..] => Call::Ask(
            "pane.list",
            with(json!({}), "workspace_id", rest(2).value("--workspace")),
        ),
        ["pane", "get", ..] => Call::Ask(
            "pane.get",
            with(
                json!({}),
                "pane_id",
                rest(2).plain.first().copied().or(rest(2).value("--pane")),
            ),
        ),
        ["pane", "current", ..] => Call::Ask("pane.current", json!({})),
        ["pane", "read", ..] => {
            let w = rest(2);
            let pane = w.plain.first().copied().or(w.value("--pane"));
            // The CLI spells sources with a dash, the socket with an underscore.
            let source = w.value("--source").unwrap_or("recent").replace('-', "_");
            let mut params = with(json!({"source": source}), "pane_id", pane);
            if let Some(lines) = w.value("--lines") {
                let lines: u64 = lines
                    .parse()
                    .map_err(|_| format!("invalid value for --lines: {lines}"))?;
                params["lines"] = json!(lines);
            }
            Call::AskText(
                "pane.read",
                with(params, "format", w.value("--format").or(Some("text"))),
            )
        }
        ["pane", "process-info", ..] => {
            let w = rest(2);
            Call::Ask(
                "pane.process_info",
                with(
                    json!({}),
                    "pane_id",
                    w.value("--pane").or(w.plain.first().copied()),
                ),
            )
        }
        ["pane", "layout", ..] => {
            let w = rest(2);
            Call::Ask(
                "pane.layout",
                with(
                    json!({}),
                    "pane_id",
                    w.value("--pane").or(w.plain.first().copied()),
                ),
            )
        }
        ["pane", "resize", ..] => {
            let w = rest(2);
            let mut params = json!({"direction": w.value("--direction").unwrap_or("left")});
            if let Some(amount) = w.value("--amount") {
                let amount: f64 = amount
                    .parse()
                    .map_err(|_| format!("invalid value '{amount}' for '--amount'"))?;
                params["amount"] = json!(amount);
            }
            let pane = w.value("--pane").or(w.plain.first().copied());
            Call::Ask("pane.resize", with(params, "pane_id", pane))
        }
        // `pane swap [--pane P] --direction D`, or `--source-pane S --target-pane T`.
        ["pane", "swap", ..] => {
            let w = rest(2);
            let params = with(json!({}), "direction", w.value("--direction"));
            let params = with(
                params,
                "pane_id",
                w.value("--pane").or(w.plain.first().copied()),
            );
            let params = with(params, "source_pane_id", w.value("--source-pane"));
            Call::Ask(
                "pane.swap",
                with(params, "target_pane_id", w.value("--target-pane")),
            )
        }
        ["pane", "report-metadata", ..] => {
            let w = rest(2);
            let params = with(metadata(&w, true)?, "pane_id", w.plain.first().copied());
            Call::Quiet("pane.report_metadata", params)
        }
        ["tab", "list", ..] => Call::Ask(
            "tab.list",
            with(json!({}), "workspace_id", rest(2).value("--workspace")),
        ),
        ["tab", "get", ..] => Call::Ask(
            "tab.get",
            with(json!({}), "tab_id", rest(2).plain.first().copied()),
        ),
        ["workspace", "list", ..] => Call::Ask("workspace.list", json!({})),
        ["workspace", "get", ..] => Call::Ask(
            "workspace.get",
            with(json!({}), "workspace_id", rest(2).plain.first().copied()),
        ),
        ["workspace", "report-metadata", ..] => {
            let w = rest(2);
            let params = with(
                metadata(&w, false)?,
                "workspace_id",
                w.plain.first().copied(),
            );
            Call::Quiet("workspace.report_metadata", params)
        }
        ["agent", "list", ..] => Call::Ask("agent.list", json!({})),
        ["agent", "get", ..] => Call::Ask(
            "agent.get",
            with(json!({}), "target", rest(2).plain.first().copied()),
        ),
        ["plugin", "list", ..] => {
            let w = rest(2);
            Call::Print(plugin_list(w.value("--plugin"), w.has("--json"))?)
        }
        ["plugin", "action", "list", ..] => Call::Ask(
            "plugin.action.list",
            with(json!({}), "plugin_id", rest(3).value("--plugin")),
        ),
        ["plugin", "action", "invoke", ..] => {
            let w = rest(3);
            let params = json!({"context": {"invocation_source": "cli"}});
            let params = with(params, "action_id", w.plain.first().copied());
            Call::Ask(
                "plugin.action.invoke",
                with(params, "plugin_id", w.value("--plugin")),
            )
        }
        ["plugin", "config-dir", more @ ..] => {
            let [id] = more else {
                return Err("usage: herdr plugin config-dir <plugin_id>".into());
            };
            if !super::manifest::valid_id(id) {
                return Err("usage: herdr plugin config-dir <plugin_id>".into());
            }
            let home = Home::here().map_err(|e| e.to_string())?;
            for dir in [home.config_of(id), home.state_of(id)] {
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            }
            Call::Print(format!("{}\n", home.config_of(id).display()))
        }
        ["plugin", "pane", "open", ..] => {
            let w = rest(3);
            let mut params = json!({"env": w.env()?});
            for (option, key) in [
                ("--plugin", "plugin_id"),
                ("--entrypoint", "entrypoint"),
                ("--placement", "placement"),
                ("--direction", "direction"),
                ("--target-pane", "target_pane_id"),
                ("--workspace", "workspace_id"),
                ("--cwd", "cwd"),
                ("--width", "width"),
                ("--height", "height"),
            ] {
                params = with(params, key, w.value(option));
            }
            // The CLI opens a plugin pane focused unless told not to.
            params["focus"] = json!(w.focus().unwrap_or(true));
            Call::Ask("plugin.pane.open", params)
        }
        ["plugin", "pane", "close", ..] => Call::Ask(
            "plugin.pane.close",
            with(json!({}), "pane_id", rest(3).plain.first().copied()),
        ),
        ["server", "reload-config", ..] => Call::Ask("server.reload_config", json!({})),
        [noun, verb, ..] if writes(noun, verb) => write(noun, verb, &rest(2), tail(2), caller)?,
        _ => {
            return Err(format!(
                "unknown command: {}\nrun 'herdr --help' for usage",
                words.join(" ")
            ));
        }
    };
    Ok(call)
}

/// The CLI's writing verbs the pinned plugins use.
fn writes(noun: &str, verb: &str) -> bool {
    matches!(
        (noun, verb),
        (
            "pane",
            "split" | "run" | "close" | "focus" | "send-text" | "send-keys" | "rename" | "zoom"
        ) | ("tab", "create" | "rename" | "focus" | "close")
            | ("workspace", "create" | "focus" | "rename" | "close")
            | ("agent", "focus" | "prompt" | "rename")
            | ("notification", "show")
    )
}

/// A writing verb as the host is asked it.
fn write(
    noun: &str,
    verb: &str,
    w: &Words,
    raw: &[String],
    caller: Option<&str>,
) -> Result<Call, String> {
    let id = w.plain.first().copied();
    let focus = |mut params: Value| {
        params["focus"] = json!(w.focus().unwrap_or(false));
        params
    };
    // A label of every word after the id, flags and all, as herdr reads it.
    let joined = || raw.get(1..).filter(|l| !l.is_empty()).map(|l| l.join(" "));
    let call = match (noun, verb) {
        ("pane", "split") => {
            let pane = pane_option(raw, caller, true)?;
            let mut params = json!({"env": w.env()?});
            params = with(params, "target_pane_id", pane.as_deref().or(caller));
            params["direction"] = json!(w.value("--direction").unwrap_or("right"));
            if let Some(ratio) = w.value("--ratio") {
                let ratio: f64 = ratio
                    .parse()
                    .map_err(|_| format!("invalid value for --ratio: {ratio}"))?;
                params["ratio"] = json!(ratio);
            }
            Call::Ask("pane.split", focus(with(params, "cwd", w.value("--cwd"))))
        }
        // `pane run` is its command typed, then Enter.
        ("pane", "run") => Call::Quiet(
            "pane.send_input",
            with(
                json!({"text": rest_text(raw)?, "keys": ["Enter"]}),
                "pane_id",
                raw.first().map(String::as_str),
            ),
        ),
        ("pane", "send-text") => Call::Quiet(
            "pane.send_text",
            with(
                json!({"text": rest_text(raw)?}),
                "pane_id",
                raw.first().map(String::as_str),
            ),
        ),
        ("pane", "send-keys") => {
            let keys: Vec<&str> = raw.iter().skip(1).map(String::as_str).collect();
            if keys.is_empty() {
                return Err("missing required keys".into());
            }
            Call::Quiet(
                "pane.send_input",
                with(
                    json!({"keys": keys}),
                    "pane_id",
                    raw.first().map(String::as_str),
                ),
            )
        }
        ("pane", "close") => Call::Ask("pane.close", with(json!({}), "pane_id", id)),
        ("pane", "focus") => {
            if !w.plain.is_empty() {
                return Err(format!("unexpected argument '{}'", w.plain[0]));
            }
            let direction = w
                .value("--direction")
                .ok_or("missing required --direction")?;
            if !["left", "right", "up", "down"].contains(&direction) {
                return Err(format!("invalid pane direction: {direction}"));
            }
            let params = with(
                json!({"direction": direction}),
                "pane_id",
                w.value("--pane"),
            );
            Call::Ask("pane.focus_direction", params)
        }
        ("pane", "rename") => {
            // A lone `--clear` clears; anything else is the label, dashes and all.
            let label = match raw.get(1..) {
                Some([only]) if only == "--clear" => None,
                Some(words) if !words.is_empty() => Some(words.join(" ")),
                _ => return Err("missing required label".into()),
            };
            Call::Ask(
                "pane.rename",
                with(with(json!({}), "label", label.as_deref()), "pane_id", id),
            )
        }
        ("pane", "zoom") => {
            if w.has("--on") && w.has("--off") {
                return Err("the argument '--on' cannot be used with '--off'".into());
            }
            let mode = if w.has("--on") {
                "on"
            } else if w.has("--off") {
                "off"
            } else {
                "toggle"
            };
            let pane = pane_option(raw, caller, false)?;
            Call::Ask(
                "pane.zoom",
                with(json!({"mode": mode}), "pane_id", pane.as_deref()),
            )
        }
        ("tab", "create") => {
            let params = with(
                json!({"env": w.env()?}),
                "workspace_id",
                w.value("--workspace"),
            );
            let params = with(params, "cwd", w.value("--cwd"));
            Call::Ask(
                "tab.create",
                focus(with(params, "label", w.value("--label"))),
            )
        }
        ("tab", "rename") => {
            let label = joined().ok_or("missing required label")?;
            Call::Ask(
                "tab.rename",
                with(
                    json!({"label": label}),
                    "tab_id",
                    raw.first().map(String::as_str),
                ),
            )
        }
        ("tab", "focus") => Call::Ask("tab.focus", with(json!({}), "tab_id", id)),
        ("tab", "close") => Call::Ask("tab.close", with(json!({}), "tab_id", id)),
        ("workspace", "create") => {
            let params = with(json!({"env": w.env()?}), "cwd", w.value("--cwd"));
            Call::Ask(
                "workspace.create",
                focus(with(params, "label", w.value("--label"))),
            )
        }
        ("workspace", "focus") => Call::Ask("workspace.focus", with(json!({}), "workspace_id", id)),
        ("workspace", "rename") => {
            let label = joined().ok_or("missing required label")?;
            Call::Ask(
                "workspace.rename",
                with(
                    json!({"label": label}),
                    "workspace_id",
                    raw.first().map(String::as_str),
                ),
            )
        }
        ("workspace", "close") => Call::Ask("workspace.close", with(json!({}), "workspace_id", id)),
        ("agent", "focus") => Call::Ask("agent.focus", with(json!({}), "target", id)),
        ("agent", "prompt") => agent_prompt(raw)?,
        ("agent", "rename") => {
            // One name, or `--clear`.
            let name = match raw.get(1..) {
                Some([only]) if only == "--clear" => None,
                Some([name]) => Some(name.as_str()),
                Some([]) | None => return Err("missing required name".into()),
                Some([_, extra, ..]) => return Err(format!("unexpected argument '{extra}'")),
            };
            Call::Ask(
                "agent.rename",
                with(with(json!({}), "name", name), "target", id),
            )
        }
        ("notification", "show") => {
            if let Some(position) = w.value("--position")
                && !["top-left", "top-right", "bottom-left", "bottom-right"].contains(&position)
            {
                return Err(format!("invalid value '{position}' for '--position'"));
            }
            if let Some(sound) = w.value("--sound")
                && !["none", "done", "request"].contains(&sound)
            {
                return Err(format!("invalid value '{sound}' for '--sound'"));
            }
            let title = id.ok_or("missing required title")?;
            Call::Ask(
                "notification.show",
                with(json!({"title": title}), "body", w.value("--body")),
            )
        }
        _ => return Err(format!("unknown command: herdr {noun} {verb}")),
    };
    Ok(call)
}

/// The states `--until` names.
const STATUSES: &[&str] = &["idle", "working", "blocked", "done", "unknown"];

/// `agent prompt <target> <text> [--wait [--until S]... [--timeout MS]]`: the target and the
/// text by position (the text may be `--wait`), the options before or after them.
fn agent_prompt(options: &[String]) -> Result<Call, String> {
    let mut positional: Vec<&String> = Vec::new();
    let (mut wait, mut until, mut timeout) = (false, Vec::new(), None);
    let mut i = 0;
    while i < options.len() {
        // The text slot takes its word whatever it is; elsewhere a dashed word is an option.
        if positional.len() == 1 || !options[i].starts_with("--") {
            if positional.len() == 2 {
                return Err(format!("unexpected argument '{}'", options[i]));
            }
            positional.push(&options[i]);
            i += 1;
            continue;
        }
        let (name, inline) = match options[i].split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (options[i].as_str(), None),
        };
        let mut value = || match &inline {
            Some(v) => Ok(v.clone()),
            None => {
                i += 1;
                options
                    .get(i)
                    .cloned()
                    .ok_or(format!("missing value for {name}"))
            }
        };
        match name {
            "--wait" if inline.is_none() => wait = true,
            "--until" => {
                let status = value()?;
                if !STATUSES.contains(&status.as_str()) {
                    return Err(format!("invalid value '{status}' for '--until'"));
                }
                until.push(status);
            }
            "--timeout" => {
                let ms = value()?;
                timeout = Some(
                    ms.parse::<u64>()
                        .map_err(|_| format!("invalid value '{ms}' for '--timeout'"))?,
                );
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
        i += 1;
    }
    if !wait && (!until.is_empty() || timeout.is_some()) {
        return Err("--until and --timeout need --wait".into());
    }
    let [target, text] = positional[..] else {
        return Err("usage: herdr agent prompt <TARGET> <TEXT> [OPTIONS]".into());
    };
    let mut params = json!({"target": target, "text": text});
    if wait {
        let mut waiting = json!({});
        if !until.is_empty() {
            waiting["until"] = json!(until);
        }
        if let Some(ms) = timeout {
            waiting["timeout_ms"] = json!(ms);
        }
        params["wait"] = waiting;
    }
    Ok(Call::Ask("agent.prompt", params))
}

/// `session list`: this session only, as herdr lists its default one.
fn session_list(socket: &Path, json: bool) -> String {
    let running = std::os::unix::net::UnixStream::connect(socket).is_ok();
    let dir = socket.parent().map(Path::to_path_buf).unwrap_or_default();
    if json {
        return format!(
            "{}\n",
            json!({"sessions": [{"name": "default", "default": true, "running": running,
                                  "socket_path": socket, "session_dir": dir}]})
        );
    }
    let status = if running { "running" } else { "stopped" };
    format!(
        "{:<20} {:<8} {:<48} socket\n{:<20} {:<8} {:<48} {}\n",
        "name",
        "status",
        "directory",
        "default",
        status,
        dir.display(),
        socket.display()
    )
}

/// `plugin list`, read here from what is installed, as herdr reads it without its server.
fn plugin_list(only: Option<&str>, json: bool) -> Result<String, String> {
    let home = Home::here().map_err(|e| e.to_string())?;
    let plugins = super::host::plugins(&home, only);
    if json {
        let envelope = json!({"id": "cli:plugin:list",
                              "result": {"type": "plugin_list", "plugins": plugins}});
        return Ok(format!("{envelope}\n"));
    }
    if plugins.is_empty() {
        return Ok("No plugins installed.\n".into());
    }
    let mut out = match plugins.len() {
        1 => "1 plugin installed:\n".to_string(),
        n => format!("{n} plugins installed:\n"),
    };
    for p in &plugins {
        let id = p["plugin_id"].as_str().unwrap_or_default();
        let source = &p["source"];
        let shown = if source["kind"] == "github" {
            let subdir = source["subdir"]
                .as_str()
                .map(|s| format!("/{s}"))
                .unwrap_or_default();
            format!(
                "github:{}/{}{subdir}@{}",
                source["owner"].as_str().unwrap_or_default(),
                source["repo"].as_str().unwrap_or_default(),
                source["resolved_commit"].as_str().unwrap_or("unknown")
            )
        } else {
            format!("local:{}", p["plugin_root"].as_str().unwrap_or_default())
        };
        let enabled = if p["enabled"] == true {
            "enabled"
        } else {
            "disabled"
        };
        out += &format!(
            "- {id} ({}) {enabled} [{shown}]\n  config: {}\n",
            p["name"].as_str().unwrap_or(id),
            home.config_of(id).display()
        );
    }
    Ok(out)
}

/// One request on the host's socket, its answer as it came. A host that is not there is an
/// error of kind `NotFound` or `ConnectionRefused`, as the connect said.
pub(super) fn ask(socket: &Path, method: &str, params: Value) -> std::io::Result<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    // A prompt that waits for the turn is answered when the turn ends: its own timeout plus
    // the usual patience, or no read timeout when it waits without one.
    let patience = match params.get("wait").filter(|w| w.is_object()) {
        Some(wait) => wait["timeout_ms"]
            .as_u64()
            .map(|ms| Duration::from_millis(ms) + PATIENCE),
        None => Some(PATIENCE),
    };
    stream.set_read_timeout(patience)?;
    let request = json!({"id": "cli", "method": method, "params": params});
    stream.write_all(format!("{request}\n").as_bytes())?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// `herdr ...` as a plugin runs it. Returns the exit code.
pub(crate) fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let caller = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|p| !p.is_empty());
    let socket = super::host::shim_socket_path();
    match parse(&args, caller.as_deref(), &socket) {
        Ok(Call::Print(text)) => {
            print!("{text}");
            0
        }
        Ok(Call::Ask(method, params)) => answered(&socket, method, params, Shown::Envelope),
        Ok(Call::AskText(method, params)) => answered(&socket, method, params, Shown::Text),
        Ok(Call::Quiet(method, params)) => answered(&socket, method, params, Shown::Nothing),
        // Called wrong: plain words and exit 2, as herdr's command line says it.
        Err(message) => {
            eprintln!("{message}");
            2
        }
    }
}

/// A request with who calls it: the plugin's own pane and id, from the environment the host
/// gave it, so a call that names no pane is about the pane it comes from.
fn called(mut params: Value) -> Value {
    for (key, var) in [
        ("caller_pane_id", "HERDR_PANE_ID"),
        ("caller_plugin_id", "HERDR_PLUGIN_ID"),
    ] {
        if let (Some(fields), Ok(value)) = (params.as_object_mut(), std::env::var(var))
            && !value.is_empty()
            && !fields.contains_key(key)
        {
            fields.insert(key.to_string(), json!(value));
        }
    }
    params
}

/// What a successful answer prints.
enum Shown {
    Envelope,
    Text,
    Nothing,
}

/// Ask the host and print its answer as herdr prints it. A refusal goes to stderr with exit 1;
/// a host that is not there is herdr's `server_not_running`.
fn answered(socket: &Path, method: &str, params: Value, shown: Shown) -> i32 {
    match ask(socket, method, called(params)) {
        Ok(answer) if answer.get("error").is_some() => {
            eprintln!("{answer}");
            1
        }
        Ok(answer) => {
            match shown {
                Shown::Envelope => println!("{answer}"),
                Shown::Text => print!(
                    "{}",
                    answer["result"]["read"]["text"]
                        .as_str()
                        .unwrap_or_default()
                ),
                Shown::Nothing => {}
            }
            0
        }
        Err(e) => {
            let (code, message) = match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => (
                    "server_not_running",
                    format!(
                        "no herdr server is running at {}; run `herdr` to start or attach it",
                        socket.display()
                    ),
                ),
                _ => ("server_not_running", e.to_string()),
            };
            eprintln!("{}", translate::refuse("cli", code, &message));
            1
        }
    }
}

/// Make `<data>/plugins/.bin/herdr` point at this binary.
pub(crate) fn place(home: &Home) -> std::io::Result<()> {
    let bin = home.bin();
    std::fs::create_dir_all(&bin)?;
    let link = bin.join("herdr");
    let exe = std::env::current_exe()?;
    if std::fs::read_link(&link).ok().as_deref() == Some(exe.as_path()) {
        return Ok(());
    }
    let temp = bin.join(format!(".herdr-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    std::os::unix::fs::symlink(&exe, &temp)?;
    std::fs::rename(&temp, &link)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &[&str]) -> Vec<String> {
        line.iter().map(|s| s.to_string()).collect()
    }

    fn call(line: &[&str]) -> Result<Call, String> {
        parse(&words(line), Some("w1:p9"), Path::new("/run/herdr.sock"))
    }

    fn split(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    fn asked(line: &str) -> (&'static str, Value) {
        match call(&split(line)).expect(line) {
            Call::Ask(method, params) | Call::Quiet(method, params) => (method, params),
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn the_read_forms_become_requests() {
        assert_eq!(asked("api snapshot"), ("session.snapshot", json!({})));
        assert_eq!(asked("pane list"), ("pane.list", json!({})));
        assert_eq!(
            asked("pane list --workspace w1 --workspace w2"),
            ("pane.list", json!({"workspace_id": "w2"})),
            "the last value wins"
        );
        assert_eq!(
            asked("pane get w1:p3"),
            ("pane.get", json!({"pane_id": "w1:p3"}))
        );
        assert_eq!(asked("pane current"), ("pane.current", json!({})));
        assert_eq!(
            asked("pane process-info --pane w1:p4"),
            ("pane.process_info", json!({"pane_id": "w1:p4"}))
        );
        assert_eq!(
            asked("tab list --workspace w1"),
            ("tab.list", json!({"workspace_id": "w1"}))
        );
        assert_eq!(
            asked("agent get w1:p2"),
            ("agent.get", json!({"target": "w1:p2"}))
        );
        assert_eq!(
            asked("plugin action list"),
            ("plugin.action.list", json!({}))
        );
        assert_eq!(
            asked("server reload-config"),
            ("server.reload_config", json!({}))
        );
    }

    #[test]
    fn pane_read_prints_its_text_from_recent_by_default() {
        assert_eq!(
            call(&split("pane read w1:p2")),
            Ok(Call::AskText(
                "pane.read",
                json!({"pane_id": "w1:p2", "source": "recent", "format": "text"})
            ))
        );
        assert_eq!(
            call(&split(
                "pane read w1:p2 --source recent-unwrapped --lines 40 --format ansi"
            )),
            Ok(Call::AskText(
                "pane.read",
                json!({"pane_id": "w1:p2", "source": "recent_unwrapped", "lines": 40,
                       "format": "ansi"})
            ))
        );
        assert!(call(&split("pane read w1:p2 --lines abc")).is_err());
    }

    #[test]
    fn the_quiet_commands_and_their_text() {
        let quiet = |line: &[&str]| match call(line) {
            Ok(Call::Quiet(method, params)) => (method, params),
            other => panic!("{line:?}: {other:?}"),
        };
        assert_eq!(
            quiet(&["pane", "send-text", "w1:p2", "--", "--resume x"]),
            (
                "pane.send_text",
                json!({"pane_id": "w1:p2", "text": "-- --resume x"})
            ),
            "a `--` is text, as herdr keeps it"
        );
        assert_eq!(
            quiet(&["pane", "send-text", "w1:p2", "a", "--literal", "b"]),
            (
                "pane.send_text",
                json!({"pane_id": "w1:p2", "text": "a --literal b"})
            )
        );
        assert_eq!(
            quiet(&["pane", "send-text", "w1:p2", ""]),
            ("pane.send_text", json!({"pane_id": "w1:p2", "text": ""})),
            "empty text is text"
        );
        assert_eq!(
            quiet(&["pane", "run", "w1:p2", "npm", "test"]),
            (
                "pane.send_input",
                json!({"pane_id": "w1:p2", "text": "npm test", "keys": ["Enter"]})
            )
        );
        assert_eq!(
            quiet(&["pane", "send-keys", "w1:p2", "Enter", "ctrl+u"]),
            (
                "pane.send_input",
                json!({"pane_id": "w1:p2", "keys": ["Enter", "ctrl+u"]})
            )
        );
        assert_eq!(
            quiet(&["pane", "report-metadata", "w1:p4", "--source", "x"]).0,
            "pane.report_metadata"
        );
        assert_eq!(
            quiet(&[
                "pane",
                "report-metadata",
                "w1:p4",
                "--source",
                "t.sb",
                "--token",
                "role=sidebar",
                "--token",
                "x=1",
                "--clear-token",
                "x",
                "--seq",
                "7",
                "--ttl-ms",
                "5000",
            ]),
            (
                "pane.report_metadata",
                json!({"pane_id": "w1:p4", "source": "t.sb", "seq": 7, "ttl_ms": 5000,
                       "tokens": {"role": "sidebar", "x": null}})
            )
        );
        assert_eq!(
            quiet(&[
                "workspace",
                "report-metadata",
                "w1",
                "--source",
                "s",
                "--token",
                "k=v"
            ]),
            (
                "workspace.report_metadata",
                json!({"workspace_id": "w1", "source": "s", "tokens": {"k": "v"}})
            )
        );
        assert!(call(&["workspace", "report-metadata", "w1", "--source", "s"]).is_err());
        assert!(call(&["pane", "report-metadata", "w1:p4", "--token", "a=b"]).is_err());
        assert!(
            call(&["pane", "send-text", "w1:p2"]).is_err(),
            "no text at all"
        );
    }

    #[test]
    fn the_writing_forms_become_requests() {
        assert_eq!(
            asked("pane split w1:p2 --direction down --ratio 0.3 --focus --no-focus --env A=1"),
            (
                "pane.split",
                json!({"target_pane_id": "w1:p2", "direction": "down", "ratio": 0.3,
                       "focus": false, "env": {"A": "1"}})
            )
        );
        assert_eq!(
            asked("pane split").1["target_pane_id"],
            "w1:p9",
            "no pane named is the caller's"
        );
        assert_eq!(asked("pane split --current").1["target_pane_id"], "w1:p9");
        assert_eq!(asked("pane split").1["focus"], false, "the CLI default");
        assert!(call(&split("pane split --env =x")).is_err());
        assert!(call(&split("pane split --env novalue")).is_err());
        assert_eq!(
            asked("pane rename w1:p2 two --words here"),
            (
                "pane.rename",
                json!({"pane_id": "w1:p2", "label": "two --words here"})
            )
        );
        assert_eq!(
            asked("pane rename w1:p2 --clear"),
            ("pane.rename", json!({"pane_id": "w1:p2"}))
        );
        assert_eq!(
            asked("tab rename w1:t3 my files"),
            (
                "tab.rename",
                json!({"tab_id": "w1:t3", "label": "my files"})
            )
        );
        assert_eq!(
            asked("tab rename w1:t3 --clear --x"),
            (
                "tab.rename",
                json!({"tab_id": "w1:t3", "label": "--clear --x"})
            ),
            "flags are words of the label"
        );
        assert_eq!(
            asked("workspace rename w1 api --focus"),
            (
                "workspace.rename",
                json!({"workspace_id": "w1", "label": "api --focus"})
            )
        );
        assert!(call(&split("agent rename w1:p2 a b")).is_err(), "one name");
        assert_eq!(
            asked("pane zoom --current --on"),
            ("pane.zoom", json!({"mode": "on"}))
        );
        assert!(call(&split("pane zoom --on --off")).is_err());
        assert_eq!(
            asked("tab create --workspace w1 --label Files --focus --env K=V"),
            (
                "tab.create",
                json!({"workspace_id": "w1", "label": "Files", "focus": true, "env": {"K": "V"}})
            )
        );
        assert_eq!(
            asked("workspace create --cwd /src --env K=V").1,
            json!({"cwd": "/src", "focus": false, "env": {"K": "V"}})
        );
        assert_eq!(
            asked("notification show done --body all --sound none --position top-left"),
            ("notification.show", json!({"title": "done", "body": "all"}))
        );
        assert!(call(&split("notification show done --sound loud")).is_err());
        assert_eq!(
            asked(
                "plugin pane open --plugin p --entrypoint e --placement popup --env K=V --env L=W"
            ),
            (
                "plugin.pane.open",
                json!({"plugin_id": "p", "entrypoint": "e", "placement": "popup",
                       "focus": true, "env": {"K": "V", "L": "W"}})
            )
        );
        assert_eq!(
            asked("plugin action invoke update --plugin herdr.collie"),
            (
                "plugin.action.invoke",
                json!({"action_id": "update", "plugin_id": "herdr.collie",
                       "context": {"invocation_source": "cli"}})
            )
        );
        assert_eq!(
            asked("pane focus --direction left --pane w1:p2"),
            (
                "pane.focus_direction",
                json!({"direction": "left", "pane_id": "w1:p2"})
            )
        );
        assert!(call(&split("pane focus w1:p2")).is_err(), "no id form");
    }

    #[test]
    fn agent_prompt_takes_one_text_and_waits_when_told() {
        assert_eq!(
            call(&["agent", "prompt", "w1:p2", "--resume please"]),
            Ok(Call::Ask(
                "agent.prompt",
                json!({"target": "w1:p2", "text": "--resume please"})
            ))
        );
        assert_eq!(
            call(&[
                "agent",
                "prompt",
                "w1:p2",
                "go",
                "--wait",
                "--until",
                "done",
                "--timeout",
                "500"
            ]),
            Ok(Call::Ask(
                "agent.prompt",
                json!({"target": "w1:p2", "text": "go",
                       "wait": {"until": ["done"], "timeout_ms": 500}})
            ))
        );
        assert!(
            call(&["agent", "prompt", "w1:p2", "a", "b"]).is_err(),
            "one text"
        );
        assert!(call(&["agent", "prompt", "w1:p2", "a", "--timeout", "5"]).is_err());
        assert_eq!(
            call(&["agent", "prompt", "w1:p2", "--wait"]),
            Ok(Call::Ask(
                "agent.prompt",
                json!({"target": "w1:p2", "text": "--wait"})
            )),
            "the text slot is read by position"
        );
        assert_eq!(
            call(&[
                "agent",
                "prompt",
                "--wait",
                "--until=done",
                "w1:p2",
                "go",
                "--timeout",
                "9"
            ]),
            Ok(Call::Ask(
                "agent.prompt",
                json!({"target": "w1:p2", "text": "go",
                       "wait": {"until": ["done"], "timeout_ms": 9}})
            )),
            "options may come before the target"
        );
        assert!(
            call(&["agent", "prompt", "--wait", "w1:p2"]).is_err(),
            "no text"
        );
        assert_eq!(
            call(&[
                "agent",
                "prompt",
                "w1:p2",
                "--wait",
                "--timeout=0",
                "--wait"
            ]),
            Ok(Call::Ask(
                "agent.prompt",
                json!({"target": "w1:p2", "text": "--wait", "wait": {"timeout_ms": 0}})
            ))
        );
        assert!(
            call(&[
                "agent", "prompt", "w1:p2", "go", "--wait", "--until", "finished"
            ])
            .is_err()
        );
    }

    #[test]
    fn the_version_and_session_list_are_answered_here() {
        assert_eq!(
            call(&["--version"]),
            Ok(Call::Print("herdr 0.9.5\n".into()))
        );
        assert!(call(&["version"]).is_err(), "no bare `version`");
        let Ok(Call::Print(text)) = call(&["session", "list", "--json"]) else {
            panic!("printed here");
        };
        let sessions: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(sessions["sessions"][0]["name"], "default");
        assert_eq!(sessions["sessions"][0]["socket_path"], "/run/herdr.sock");
        assert_eq!(sessions["sessions"][0]["session_dir"], "/run");
        let Ok(Call::Print(table)) = call(&["session", "list"]) else {
            panic!("printed here");
        };
        assert!(
            table.starts_with("name                 status   directory"),
            "{table}"
        );
        assert!(call(&["session", "list", "--all"]).is_err());
    }

    #[test]
    fn an_unknown_command_is_refused() {
        let err = call(&["integration", "status"]).expect_err("not served");
        assert!(
            err.starts_with("unknown command: integration status"),
            "{err}"
        );
        assert!(call(&["machine", "list", "--json"]).is_err());
        assert!(call(&["plugin", "config-dir", "../x"]).is_err());
        assert!(
            call(&["plugin", "config-dir", "a", "b"]).is_err(),
            "exactly one"
        );
    }

    #[test]
    fn only_the_plugins_bin_is_the_shim() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = dir.path().join(".bin");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::create_dir_all(&other).expect("mkdir");
        for folder in [&bin, &other] {
            std::os::unix::fs::symlink("/bin/sh", folder.join("herdr")).expect("link");
        }
        assert!(is_shim(Some(&bin.join("herdr")), &bin));
        assert!(
            !is_shim(Some(&other.join("herdr")), &bin),
            "the person's own herdr"
        );
        assert!(!is_shim(Some(&bin.join("qoop")), &bin));
        assert!(!is_shim(None, &bin));
    }
}

//! `herdr`, as a plugin runs it: the qoop binary under the name `herdr` in
//! `<data>/plugins/.bin`, which is first on a plugin's `PATH` and is its `HERDR_BIN_PATH`.
//!
//! It turns herdr's CLI forms into requests on the plugin host's socket and prints herdr's
//! envelope back: the answer on stdout, a refusal on stderr with a failing exit. Only that path
//! is ever dispatched here, so a `herdr` elsewhere on the machine (the person's own) is never
//! shadowed outside a plugin's environment.

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

/// What a CLI form asks: an answer printed here, or a request for the host.
#[derive(Debug, PartialEq)]
enum Call {
    Print(String),
    Ask(&'static str, Value),
    /// `pane read --format`: the text itself on stdout, as herdr prints it.
    AskText(&'static str, Value),
}

/// The CLI's options that take a value, so their values are never read as positionals.
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
    "--title",
    "--token",
    "--clear-token",
    "--agent",
    "--seq",
    "--ttl-ms",
    "--split",
];

/// Positionals and options of the words after a command.
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

    fn value(&self, name: &str) -> Option<&'a str> {
        self.options
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| *v)
    }

    fn has(&self, name: &str) -> bool {
        self.options.iter().any(|(n, _)| *n == name)
    }

    /// The first positional, else the named option.
    fn target(&self, option: &str) -> Option<&'a str> {
        self.plain.first().copied().or_else(|| self.value(option))
    }
}

/// Put `key: value` into `params` when there is a value.
fn with(mut params: Value, key: &str, value: Option<&str>) -> Value {
    if let Some(value) = value {
        params[key] = json!(value);
    }
    params
}

/// A herdr CLI form, as the host is asked it. `caller` is the plugin's own pane
/// (`HERDR_PANE_ID`), which `pane current` falls back to.
fn parse(args: &[String], caller: Option<&str>, socket: &Path) -> Result<Call, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let rest = |n: usize| Words::of(&args[n.min(args.len())..]);
    let call = match words.as_slice() {
        ["--version" | "-V" | "version", ..] => {
            Call::Print(format!("herdr {}\n", translate::VERSION))
        }
        ["api", "snapshot", ..] => Call::Ask("session.snapshot", json!({})),
        ["session", "list", ..] => {
            let socket = socket.display().to_string();
            if rest(2).has("--json") {
                let sessions = json!([{"name": "default", "default": true, "running": true,
                                       "socket_path": socket}]);
                Call::Print(format!(
                    "{}\n",
                    translate::reply("cli", json!({"type": "session_list", "sessions": sessions}))
                ))
            } else {
                Call::Print("default running\n".to_string())
            }
        }
        ["pane", "list", ..] => Call::Ask(
            "pane.list",
            with(json!({}), "workspace_id", rest(2).value("--workspace")),
        ),
        ["pane", "get", ..] => Call::Ask(
            "pane.get",
            with(json!({}), "pane_id", rest(2).target("--pane")),
        ),
        ["pane", "current", ..] => {
            Call::Ask("pane.current", with(json!({}), "caller_pane_id", caller))
        }
        ["pane", "read", ..] => {
            let w = rest(2);
            let mut params = with(json!({}), "pane_id", w.target("--pane"));
            // The CLI spells sources with a dash, the socket with an underscore.
            let source = w.value("--source").unwrap_or("visible").replace('-', "_");
            params["source"] = json!(source);
            if let Some(lines) = w.value("--lines").and_then(|n| n.parse::<u64>().ok()) {
                params["lines"] = json!(lines);
            }
            match w.value("--format") {
                Some(format) => Call::AskText("pane.read", with(params, "format", Some(format))),
                None => Call::Ask("pane.read", params),
            }
        }
        ["pane", "process-info", ..] => Call::Ask(
            "pane.process_info",
            with(json!({}), "pane_id", rest(2).target("--pane")),
        ),
        ["pane", "layout", ..] => Call::Ask(
            "pane.layout",
            with(json!({}), "pane_id", rest(2).target("--pane")),
        ),
        ["pane", "resize", ..] => Call::Ask(
            "pane.resize",
            json!({"direction": rest(2).value("--direction").unwrap_or("right")}),
        ),
        ["pane", "report-metadata", ..] => {
            let w = rest(2);
            let params = with(json!({}), "pane_id", w.target("--pane"));
            Call::Ask(
                "pane.report_metadata",
                with(params, "source", w.value("--source")),
            )
        }
        ["tab", "list", ..] => Call::Ask(
            "tab.list",
            with(json!({}), "workspace_id", rest(2).value("--workspace")),
        ),
        ["tab", "get", ..] => Call::Ask(
            "tab.get",
            with(json!({}), "tab_id", rest(2).target("--tab")),
        ),
        ["workspace", "list", ..] => Call::Ask("workspace.list", json!({})),
        ["workspace", "get", ..] => Call::Ask(
            "workspace.get",
            with(json!({}), "workspace_id", rest(2).target("--workspace")),
        ),
        ["workspace", "report-metadata", ..] => Call::Ask(
            "workspace.report_metadata",
            with(json!({}), "workspace_id", rest(2).target("--workspace")),
        ),
        ["agent", "list", ..] => Call::Ask("agent.list", json!({})),
        ["agent", "get", ..] => Call::Ask(
            "agent.get",
            with(json!({}), "target", rest(2).target("--pane")),
        ),
        ["plugin", "list", ..] => Call::Ask(
            "plugin.list",
            with(json!({}), "plugin_id", rest(2).value("--plugin")),
        ),
        ["plugin", "action", "list", ..] => Call::Ask(
            "plugin.action.list",
            with(json!({}), "plugin_id", rest(3).value("--plugin")),
        ),
        ["plugin", "config-dir", id, ..] => {
            if !super::manifest::valid_id(id) {
                return Err(format!("{id:?} is not a plugin id"));
            }
            let home = Home::here().map_err(|e| e.to_string())?;
            let dir = home.config_of(id);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Call::Print(format!("{}\n", dir.display()))
        }
        ["server", "reload-config", ..] => Call::Ask("server.reload_config", json!({})),
        _ => return Err(format!("unknown command: herdr {}", words.join(" "))),
    };
    Ok(call)
}

/// One request on the host's socket, its answer as it came.
fn ask(socket: &Path, method: &str, params: Value) -> std::io::Result<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(|e| {
        std::io::Error::other(format!(
            "the qoop plugin host is not answering on {}: {e}",
            socket.display()
        ))
    })?;
    stream.set_read_timeout(Some(PATIENCE))?;
    let request = json!({"id": "cli", "method": method, "params": params});
    stream.write_all(format!("{request}\n").as_bytes())?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// `herdr ...` as a plugin runs it. Returns the exit code.
pub(crate) fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let caller = std::env::var("HERDR_PANE_ID").ok();
    let socket = super::host::socket_path();
    let refused = |code: &str, message: &str| {
        eprintln!("{}", translate::refuse("cli", code, message));
        1
    };
    match parse(&args, caller.as_deref(), &socket) {
        Ok(Call::Print(text)) => {
            print!("{text}");
            0
        }
        Ok(Call::Ask(method, params)) => answered(&socket, method, params, false),
        Ok(Call::AskText(method, params)) => answered(&socket, method, params, true),
        Err(message) => refused("invalid_request", &message),
    }
}

/// Ask the host and print its answer: the envelope, or for `text` the read text alone. A
/// refusal goes to stderr with a failing exit.
fn answered(socket: &Path, method: &str, params: Value, text: bool) -> i32 {
    match ask(socket, method, params) {
        Ok(answer) if answer.get("error").is_some() => {
            eprintln!("{answer}");
            1
        }
        Ok(answer) if text => {
            println!(
                "{}",
                answer["result"]["read"]["text"]
                    .as_str()
                    .unwrap_or_default()
            );
            0
        }
        Ok(answer) => {
            println!("{answer}");
            0
        }
        Err(e) => {
            let refused = translate::refuse("cli", "server_unavailable", &e.to_string());
            eprintln!("{refused}");
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

    fn call(words: &str) -> Result<Call, String> {
        let args: Vec<String> = words.split_whitespace().map(String::from).collect();
        parse(&args, Some("w1:p9"), Path::new("/run/herdr.sock"))
    }

    fn asked(words: &str) -> (&'static str, Value) {
        match call(words).expect(words) {
            Call::Ask(method, params) => (method, params),
            other => panic!("{words}: {other:?}"),
        }
    }

    #[test]
    fn the_cli_forms_the_pinned_plugins_use_become_requests() {
        assert_eq!(asked("api snapshot"), ("session.snapshot", json!({})));
        assert_eq!(asked("pane list"), ("pane.list", json!({})));
        assert_eq!(
            asked("pane list --workspace w2"),
            ("pane.list", json!({"workspace_id": "w2"}))
        );
        assert_eq!(
            asked("pane get w1:p3"),
            ("pane.get", json!({"pane_id": "w1:p3"}))
        );
        assert_eq!(
            asked("pane current"),
            ("pane.current", json!({"caller_pane_id": "w1:p9"}))
        );
        assert_eq!(
            call("pane read w1:p2 --source recent-unwrapped --lines 40 --format text"),
            Ok(Call::AskText(
                "pane.read",
                json!({"pane_id": "w1:p2", "source": "recent_unwrapped", "lines": 40,
                       "format": "text"})
            )),
            "the text alone is printed, the source spelt as the socket spells it"
        );
        assert_eq!(
            asked("pane read w1:p2"),
            (
                "pane.read",
                json!({"pane_id": "w1:p2", "source": "visible"})
            )
        );
        assert_eq!(
            asked("pane process-info --pane w1:p4"),
            ("pane.process_info", json!({"pane_id": "w1:p4"}))
        );
        assert_eq!(
            asked("pane layout --pane w1:p4"),
            ("pane.layout", json!({"pane_id": "w1:p4"}))
        );
        assert_eq!(
            asked("pane resize --pane w1:p4 --direction left --amount 0"),
            ("pane.resize", json!({"direction": "left"}))
        );
        assert_eq!(
            asked("pane report-metadata w1:p4 --source x --token topic=y"),
            (
                "pane.report_metadata",
                json!({"pane_id": "w1:p4", "source": "x"})
            )
        );
        assert_eq!(
            asked("tab list --workspace w1"),
            ("tab.list", json!({"workspace_id": "w1"}))
        );
        assert_eq!(
            asked("tab get w1:t3"),
            ("tab.get", json!({"tab_id": "w1:t3"}))
        );
        assert_eq!(asked("workspace list"), ("workspace.list", json!({})));
        assert_eq!(
            asked("workspace get w2"),
            ("workspace.get", json!({"workspace_id": "w2"}))
        );
        assert_eq!(asked("agent list"), ("agent.list", json!({})));
        assert_eq!(
            asked("agent get w1:p2"),
            ("agent.get", json!({"target": "w1:p2"}))
        );
        assert_eq!(
            asked("plugin list --plugin annotate --json"),
            ("plugin.list", json!({"plugin_id": "annotate"}))
        );
        assert_eq!(asked("plugin list --json"), ("plugin.list", json!({})));
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
    fn the_version_and_session_list_are_answered_here() {
        assert_eq!(call("--version"), Ok(Call::Print("herdr 0.9.5\n".into())));
        let Ok(Call::Print(json_text)) = call("session list --json") else {
            panic!("printed here");
        };
        let sessions: Value = serde_json::from_str(&json_text).expect("json");
        assert_eq!(sessions["result"]["sessions"][0]["name"], "default");
        assert_eq!(
            sessions["result"]["sessions"][0]["socket_path"],
            "/run/herdr.sock"
        );
        assert_eq!(
            call("session list"),
            Ok(Call::Print("default running\n".into()))
        );
    }

    #[test]
    fn an_unknown_command_is_refused() {
        let err = call("integration status").expect_err("not served");
        assert_eq!(err, "unknown command: herdr integration status");
        assert!(call("machine list --json").is_err());
        assert!(call("plugin config-dir ../x").is_err());
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

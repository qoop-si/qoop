//! herdr plugins as the interface meets them: their actions in the palette, run through the
//! plugin host, and the terminal on screen told to the host. The interface never starts the
//! host; with none running it sends nothing and lists nothing.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

/// How long one question to the host may take before it counts as not there.
const PATIENCE: Duration = Duration::from_secs(2);
/// A focus report not taken is tried again after this, then every [`FOCUS_LATER`], for at
/// most [`FOCUS_MOST`] in all: a host that starts late still learns what is on screen.
const FOCUS_SOON: Duration = Duration::from_millis(300);
const FOCUS_LATER: Duration = Duration::from_secs(2);
const FOCUS_MOST: Duration = Duration::from_secs(30);

/// Why the host gave no answer, or the answer it gave instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failed {
    /// No host answers on the socket.
    HostDown,
    /// The host refused, or answered something unreadable: what it said.
    Said(String),
}
/// The notice for that, with what to do about it. Fits 60 columns.
pub const HOST_DOWN_NOTICE: &str = "plugin host is not running: quit and run qoop again";
/// The most columns of an action's title the notice row gives it.
const TITLE_MOST: usize = 32;

/// One plugin action, as the host lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub plugin: String,
    pub action: String,
    pub title: String,
    /// herdr's `contexts`: global, workspace, tab, pane, selection. Empty is global.
    pub contexts: Vec<String>,
}

/// What the palette has heard about plugin actions since it opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Listed {
    /// Not answered yet, or no plugin here: nothing is shown.
    #[default]
    Nothing,
    Actions(Vec<Action>),
    /// Plugins are installed but no host answers.
    HostDown,
}

/// Whether an action belongs in a palette opened over `focused`. A pane, tab or space
/// action needs a terminal of this machine's daemon, the only one the host acts on; a
/// selection action needs a selection, which the palette never has.
pub fn shown_for(contexts: &[String], focused: u32) -> bool {
    let local = focused != 0 && !crate::mux::is_remote_terminal(focused) && crate::mux::has_local();
    contexts.is_empty()
        || contexts.iter().any(|c| match c.as_str() {
            "global" => true,
            "workspace" | "tab" | "pane" => local,
            _ => false,
        })
}

/// One request on the host's socket and its `result`, or why there is none. Blocking: run
/// it off the render path.
fn ask(method: &str, params: Value) -> Result<Value, Failed> {
    let down = |_| Failed::HostDown;
    let mut stream =
        std::os::unix::net::UnixStream::connect(qoop_runtime::paths::plugin_host_socket())
            .map_err(down)?;
    stream.set_read_timeout(Some(PATIENCE)).map_err(down)?;
    stream.set_write_timeout(Some(PATIENCE)).map_err(down)?;
    let request = json!({"id": "qoop-tui", "method": method, "params": params});
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(down)?;
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| Failed::Said(format!("the plugin host did not answer: {e}")))?;
    let answer: Value = serde_json::from_str(&line)
        .map_err(|_| Failed::Said("the plugin host answered something unreadable".into()))?;
    match answer.get("error") {
        Some(error) => Err(Failed::Said(
            error["message"]
                .as_str()
                .unwrap_or("the plugin host refused")
                .to_string(),
        )),
        None => Ok(answer["result"].clone()),
    }
}

/// Whether any plugin is installed here, read from its folder without the host.
fn installed() -> bool {
    let Some(data) = qoop_runtime::paths::data_dir() else {
        return false;
    };
    std::fs::read_dir(data.join("plugins")).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            !e.file_name().to_string_lossy().starts_with('.')
                && std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir())
        })
    })
}

/// The actions of every plugin still agreed to, from the host. Blocking.
pub fn list() -> Listed {
    match ask("plugin.action.list", json!({})) {
        Ok(result) => Listed::Actions(actions_of(&result)),
        Err(_) if installed() => Listed::HostDown,
        Err(_) => Listed::Nothing,
    }
}

fn actions_of(result: &Value) -> Vec<Action> {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    result["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| Action {
            plugin: text(&a["plugin_id"]),
            action: text(&a["action_id"]),
            title: text(&a["title"]),
            contexts: a["contexts"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect(),
        })
        .filter(|a| !a.plugin.is_empty() && !a.action.is_empty())
        .collect()
}

/// Run one action through the host, about `focused`: the host is told that terminal first,
/// so the context the action gets is the one the palette was opened over. Blocking.
pub fn run(plugin: &str, action: &str, focused: u32) -> Result<(), Failed> {
    if focused != 0 && !crate::mux::is_remote_terminal(focused) {
        // A terminal the host does not list yet is no reason not to run.
        let _ = ask("qoop.focus", json!({"terminal": focused}));
    }
    // herdr runs an action a person picks from a key binding; the palette is that here.
    ask(
        "plugin.action.invoke",
        json!({"plugin_id": plugin, "action_id": action,
               "context": {"invocation_source": "keybinding"}}),
    )
    .map(drop)
}

/// An action as the notice row names it: its title with nothing a terminal would act on,
/// cut so the plugin's id still shows, then that id.
pub fn named(title: &str, plugin: &str) -> String {
    let title: String = title.chars().filter(|c| crate::width::drawn(*c)).collect();
    let title = if crate::width::width(&title) > TITLE_MOST {
        format!(
            "{}\u{2026}",
            crate::width::text(&title, TITLE_MOST - 1).trim_end()
        )
    } else {
        title
    };
    format!("{title} ({plugin})")
}

/// What the notice row says about a picked action once the host answered.
pub fn ran_notice(named: &str, ran: &Result<(), Failed>) -> String {
    match ran {
        Ok(()) => format!("started {named}"),
        Err(Failed::HostDown) => HOST_DOWN_NOTICE.to_string(),
        // What to do first, so a narrow row keeps it; the host's reason after.
        Err(Failed::Said(why)) => format!("did not start (see qoop plugin ls): {why}"),
    }
}

/// The terminal to tell the host about now, if any: one that changed since `last`, is not
/// zero and belongs to this machine's daemon (a remote one is numbered by the interface,
/// not by any daemon the host follows).
pub fn focus_to_report(focused: u32, last: &mut u32) -> Option<u32> {
    if focused == 0
        || focused == *last
        || crate::mux::is_remote_terminal(focused)
        || !crate::mux::has_local()
    {
        return None;
    }
    *last = focused;
    Some(focused)
}

/// How long to wait before telling the host again, after `waited` in all; `None` once
/// [`FOCUS_MOST`] would pass.
fn focus_pause(waited: Duration) -> Option<Duration> {
    let pause = if waited.is_zero() {
        FOCUS_SOON
    } else {
        FOCUS_LATER
    };
    (waited + pause <= FOCUS_MOST).then_some(pause)
}

/// Tells the host which terminal is on screen, off the render path: the newest value
/// wins and an older one still on its way is skipped.
pub struct FocusReport {
    tx: tokio::sync::watch::Sender<u32>,
}

impl FocusReport {
    /// Start the sender. Inside the interface's runtime.
    pub fn start() -> Self {
        let (tx, mut rx) = tokio::sync::watch::channel(0u32);
        tokio::spawn(async move {
            if rx.changed().await.is_err() {
                return;
            }
            loop {
                let terminal = *rx.borrow_and_update();
                // A host not listening yet, restarting, or not listing a new terminal yet is
                // asked again on `focus_pause`; a newer value ends the wait at once.
                let mut waited = Duration::ZERO;
                let newer = loop {
                    let told = tokio::task::spawn_blocking(move || {
                        ask("qoop.focus", json!({"terminal": terminal}))
                    })
                    .await;
                    if matches!(told, Ok(Ok(_))) {
                        break false;
                    }
                    let Some(pause) = focus_pause(waited) else {
                        break false;
                    };
                    waited += pause;
                    tokio::select! {
                        _ = tokio::time::sleep(pause) => {}
                        changed = rx.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            break true;
                        }
                    }
                };
                if !newer && rx.changed().await.is_err() {
                    return;
                }
            }
        });
        Self { tx }
    }

    pub fn send(&self, terminal: u32) {
        let _ = self.tx.send(terminal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_shows_where_its_contexts_say() {
        let of = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let remote = crate::mux::REMOTE_TERMINAL | 1 << 24 | 3;
        assert!(shown_for(&of(&[]), 0), "no contexts is global");
        assert!(shown_for(&of(&["global"]), remote));
        assert!(shown_for(&of(&["pane"]), 4));
        assert!(shown_for(&of(&["workspace", "tab"]), 4));
        assert!(!shown_for(&of(&["pane"]), 0), "no terminal to act on");
        assert!(
            !shown_for(&of(&["pane"]), remote),
            "the host acts on local terminals"
        );
        assert!(
            !shown_for(&of(&["selection"]), 4),
            "the palette has no selection"
        );
        assert!(shown_for(&of(&["selection", "pane"]), 4));
    }

    #[test]
    fn focus_is_reported_once_per_change_and_never_for_zero_or_a_remote() {
        let mut last = 0;
        assert_eq!(focus_to_report(0, &mut last), None);
        assert_eq!(focus_to_report(3, &mut last), Some(3));
        assert_eq!(focus_to_report(3, &mut last), None, "once per change");
        let remote = crate::mux::REMOTE_TERMINAL | 2 << 24 | 3;
        assert_eq!(focus_to_report(remote, &mut last), None);
        assert_eq!(last, 3, "a remote leaves the last report standing");
        assert_eq!(focus_to_report(5, &mut last), Some(5));
        assert_eq!(
            focus_to_report(3, &mut last),
            Some(3),
            "back again is a change"
        );
    }

    #[test]
    fn a_long_or_hostile_title_is_named_short_and_clean() {
        let title = format!(
            "\x1b]0;owned\x07{}\nend",
            "very long action title ".repeat(4)
        );
        let name = named(&title, "demo.stamp");
        assert!(name.chars().all(crate::width::drawn), "{name:?}");
        assert!(name.ends_with("\u{2026} (demo.stamp)"), "{name}");
        assert!(crate::width::width(&name) <= TITLE_MOST + " (demo.stamp)".len());
        assert_eq!(named("Go", "t.a"), "Go (t.a)");
        assert_eq!(ran_notice(&name, &Err(Failed::HostDown)), HOST_DOWN_NOTICE);
        assert!(HOST_DOWN_NOTICE.len() <= 60);
        assert!(
            ran_notice(&name, &Err(Failed::Said("plugin not found".into())))
                .starts_with("did not start (see qoop plugin ls)")
        );
    }

    #[test]
    fn a_focus_report_is_tried_again_for_thirty_seconds() {
        let mut waited = Duration::ZERO;
        let mut pauses = Vec::new();
        while let Some(pause) = focus_pause(waited) {
            pauses.push(pause);
            waited += pause;
        }
        assert_eq!(pauses[0], Duration::from_millis(300), "soon first");
        assert!(pauses[1..].iter().all(|p| *p == Duration::from_secs(2)));
        assert!(
            waited <= FOCUS_MOST && waited + FOCUS_LATER > FOCUS_MOST,
            "{waited:?}"
        );
        assert_eq!(pauses.len(), 15);
    }

    #[test]
    fn the_hosts_list_becomes_actions() {
        let result = json!({"type": "plugin_action_list", "actions": [
            {"plugin_id": "t.a", "action_id": "go", "title": "Go", "contexts": ["pane"]},
            {"plugin_id": "", "action_id": "x"},
        ]});
        assert_eq!(
            actions_of(&result),
            [Action {
                plugin: "t.a".into(),
                action: "go".into(),
                title: "Go".into(),
                contexts: vec!["pane".into()],
            }]
        );
    }
}

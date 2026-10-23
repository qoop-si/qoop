//! Which conversation a Claude Code tab shows while it is in Claude's agent view.
//!
//! In the agent view the `claude` in the tab is a host: its own conversation was moved to
//! the background (`parkedJobId` in its pid file), and the job it opens runs in a worker
//! under Claude's own daemon. The worker's hooks name the job's conversation but not the
//! tab (their `QOOP_TERMINAL` is whichever tab started Claude's daemon), and the host
//! reports nothing while a job is open. Claude writes the host to job link nowhere, so it
//! is read off the tab: the job's name in the prompt box's top border, matched against the
//! pid files of Claude's live workers. Measured on Claude Code 2.1.309.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// How far up from the bottom of the screen the prompt box's border is looked for. With a
/// dialog open the box sits above the dialog's own lines, so not only right above `❯`.
const BORDER_ROWS: usize = 15;

/// The name drawn in the top border of Claude's prompt box (`──── Some name ─`), if the
/// bottom of the screen, given as its rows, has one.
pub fn border_name(screen: &[impl AsRef<str>]) -> Option<String> {
    let end = screen
        .iter()
        .rposition(|l| !l.as_ref().trim().is_empty())
        .map_or(0, |i| i + 1);
    screen[..end]
        .iter()
        .rev()
        .take(BORDER_ROWS)
        .find_map(|line| {
            let line = line.as_ref().trim();
            let inner = line.strip_prefix('─')?.strip_suffix('─')?;
            let name = inner.trim_matches('─');
            // One run of text between the dashes, set off by a space on each side.
            let text = name.strip_prefix(' ')?.strip_suffix(' ')?;
            (!text.trim().is_empty() && !text.contains('─')).then(|| text.trim().to_string())
        })
}

/// The Claude config directory of the process `pid`: its own `CLAUDE_CONFIG_DIR`, else
/// `~/.claude`. Per process, since a tab can run Claude under another account's directory.
pub fn config_dir(pid: i32) -> Option<PathBuf> {
    if let Some(dir) =
        qoop_pty::sys::env_of(qoop_pty::Pid(pid), "CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty())
    {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude"))
}

/// The conversation the Claude process `host` shows, when it is in the agent view with a
/// job open whose name `name` is: the `sessionId` of the one live worker of that name.
/// `None` when `host` is not in the agent view, or no single worker matches.
///
/// `attach` says the host is a `claude attach <job>` client, which has no pid file of its
/// own but draws the job's screen the same way.
pub fn shown_session(config: &Path, host: i32, attach: bool, name: &str) -> Option<String> {
    let sessions = config.join("sessions");
    let parked = || {
        read(&sessions.join(format!("{host}.json")))
            .is_some_and(|f| f.get("parkedJobId").and_then(Value::as_str).is_some())
    };
    if !attach && !parked() {
        return None;
    }
    // ponytail: one small file per live Claude process, read only while a border name
    // shows and at most once a second; a pid reused after a crash that left its file is
    // taken as live, which the name match makes harmless.
    let mut found = std::fs::read_dir(&sessions).ok()?.filter_map(|entry| {
        let path = entry.ok()?.path();
        if path.extension()? != "json" {
            return None;
        }
        let file = read(&path)?;
        let text = |key: &str| file.get(key).and_then(Value::as_str);
        // A spare waiting to be claimed is named after its job id and runs nothing yet.
        if text("kind")? != "bg"
            || text("jobId").is_none()
            || file.get("spare").and_then(Value::as_bool) == Some(true)
            || !named(text("name")?, name)
        {
            return None;
        }
        let pid = i32::try_from(file.get("pid")?.as_i64()?).ok()?;
        // SAFETY: signal 0 checks that the process exists and sends nothing.
        (unsafe { libc::kill(pid, 0) } == 0).then(|| text("sessionId").map(str::to_string))?
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Whether a worker's name is the one in the border, which Claude cuts with `…` when the
/// box is too narrow for it.
fn named(worker: &str, border: &str) -> bool {
    worker == border
        || border
            .strip_suffix('…')
            .is_some_and(|head| !head.is_empty() && worker.starts_with(head.trim_end()))
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTACHED: &str = "✻ Baked for 3s\n\n\n\
        ───────────────────────────── Café deploy plan ─\n\
        ❯\n\
        ────────────────────────────────────────────────\n\
        \u{20} ⏵⏵ auto mode on (shift+tab to cycle) · ← 5 agents\n\n\n";

    fn border_name(screen: &str) -> Option<String> {
        super::border_name(&screen.lines().collect::<Vec<_>>())
    }

    #[test]
    fn the_border_name_is_read_from_the_prompt_box() {
        assert_eq!(border_name(ATTACHED).as_deref(), Some("Café deploy plan"));
        // A question dialog open, the box below it with the task list.
        let dialog = "❯ 1. Tailscale (Recommended)\n  2. Pull model\n\
            Enter to select · Tab/Arrow keys to navigate · Esc to cancel\n\
            ─────────────────────── Build server setup ─\n\n\
            \u{20} 3 tasks (0 done, 3 open)\n  ◻ Phase 0\n  ◻ Phase 1\n";
        assert_eq!(border_name(dialog).as_deref(), Some("Build server setup"));
    }

    #[test]
    fn a_screen_without_a_named_border_has_no_name() {
        // The agent view's list, and a plain session.
        let list = "Needs input\n✻ Café deploy plan      done     2s\n\n\
            ────────────────────────────────\n❯ describe a task for a new session\n\
            ────────────────────────────────\n  ⏵⏵ auto mode · enter to return\n";
        assert_eq!(border_name(list), None);
        // A divider high up in the transcript is not the prompt box.
        let mut far = String::from("── Summary ──\n");
        far.push_str(&"line\n".repeat(BORDER_ROWS + 2));
        assert_eq!(border_name(&far), None);
    }

    /// A Claude config directory laid out as Claude Code 2.1.309 lays it out.
    struct Layout(tempfile::TempDir);

    impl Layout {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a directory");
            std::fs::create_dir(dir.path().join("sessions")).expect("sessions");
            Layout(dir)
        }
        fn pid_file(&self, pid: u32, body: serde_json::Value) {
            std::fs::write(
                self.0.path().join("sessions").join(format!("{pid}.json")),
                body.to_string(),
            )
            .expect("a pid file");
        }
        fn host(&self, pid: u32, parked: bool) {
            let mut body =
                serde_json::json!({"pid": pid, "sessionId": "host-id", "kind": "interactive"});
            if parked {
                body["parkedJobId"] = "7b3e9a10".into();
            }
            self.pid_file(pid, body);
        }
        fn worker(&self, pid: u32, name: &str, session: &str) {
            self.pid_file(
                pid,
                serde_json::json!({"pid": pid, "sessionId": session, "kind": "bg",
                    "jobId": &session[..8], "name": name}),
            );
        }
    }

    /// A pid that is certainly gone: a child that has exited and been reaped.
    fn gone_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        pid
    }

    #[test]
    fn a_host_in_the_agent_view_shows_the_one_live_worker_of_that_name() {
        let layout = Layout::new();
        let live = std::process::id();
        layout.host(1, true);
        layout.worker(
            live,
            "Café deploy plan",
            "5a1e0b2c-7d3f-4e8a-9b6c-0d1e2f3a4b5c",
        );
        layout.worker(
            gone_pid(),
            "Café deploy plan",
            "deadbeef-0000-0000-0000-000000000000",
        );
        assert_eq!(
            shown_session(layout.0.path(), 1, false, "Café deploy plan").as_deref(),
            Some("5a1e0b2c-7d3f-4e8a-9b6c-0d1e2f3a4b5c")
        );
        assert_eq!(
            shown_session(layout.0.path(), 1, false, "Café dep…").as_deref(),
            Some("5a1e0b2c-7d3f-4e8a-9b6c-0d1e2f3a4b5c"),
            "a name the border cut short"
        );
        // `claude attach` has no pid file and is let through by its argv.
        assert!(shown_session(layout.0.path(), 2, true, "Café deploy plan").is_some());
    }

    #[test]
    fn nothing_is_shown_unless_the_host_and_one_worker_say_so() {
        let layout = Layout::new();
        let live = std::process::id();
        layout.worker(
            live,
            "Café deploy plan",
            "5a1e0b2c-7d3f-4e8a-9b6c-0d1e2f3a4b5c",
        );
        // A plain session whose own name happens to be a job's.
        layout.host(1, false);
        assert_eq!(
            shown_session(layout.0.path(), 1, false, "Café deploy plan"),
            None
        );
        // No pid file for the host at all.
        assert_eq!(
            shown_session(layout.0.path(), 3, false, "Café deploy plan"),
            None
        );
        layout.host(2, true);
        assert_eq!(shown_session(layout.0.path(), 2, false, "Other job"), None);
        // A spare is named after its job id and runs nothing.
        layout.pid_file(
            live + 1,
            serde_json::json!({"pid": live, "sessionId": "8a9d8077-x", "kind": "bg",
                "jobId": "8a9d8077", "name": "8a9d8077", "spare": true}),
        );
        assert_eq!(shown_session(layout.0.path(), 2, false, "8a9d8077"), None);
        // Two live workers of one name: not a guess.
        layout.pid_file(
            live + 2,
            serde_json::json!({"pid": live, "sessionId": "22182c4a-x", "kind": "bg",
                "jobId": "22182c4a", "name": "Café deploy plan"}),
        );
        assert_eq!(
            shown_session(layout.0.path(), 2, false, "Café deploy plan"),
            None
        );
    }
}

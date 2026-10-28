//! The injected shell integration, end to end: a real zsh or bash in a daemon's tab says where
//! each command starts and ends (OSC 133 C and D), and the tab's row carries the last one.
//!
//! Every test here sets the environment its daemon's shells inherit (a home of its own, and
//! `ZDOTDIR` or not), so they take turns: the lock is held from the first write to the end.

use std::path::Path;
use std::time::{Duration, Instant};

use qoop_runtime::control::{CommandRow, ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ZSH: &str = "/bin/zsh";
/// A bash with `PS0` (4.4 or newer); macOS's own `/bin/bash` is 3.2 and gets no marks.
const NEW_BASH: &str = "/opt/homebrew/bin/bash";

/// A home with the given startup files, and `ZDOTDIR` set to `zdotdir` or unset. The caller
/// holds `SERIAL`, and no daemon of this binary is running.
fn home_with(files: &[(&str, &str)], zdotdir: Option<&Path>) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    for (name, text) in files {
        let path = home.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }
    // SAFETY: the tests here take turns under `SERIAL`, and each one's daemon starts after
    // this returns; nothing else in this binary reads or writes the environment meanwhile.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("QOOP_ZDOTDIR");
        std::env::remove_var("QOOP_SHELL_INTEGRATION");
        match zdotdir {
            Some(dir) => std::env::set_var("ZDOTDIR", dir),
            None => std::env::remove_var("ZDOTDIR"),
        }
    }
    home
}

/// The last finished command on this client's default terminal, once there is one that
/// `done` accepts (a command typed earlier in the test may be the last one for a while).
async fn last_command(
    c: &mut TestClient,
    done: impl Fn(&CommandRow) -> bool,
) -> Option<CommandRow> {
    let terminal = c.default_stream();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let response = c
            .control(&ControlRequest::ListAgents { id: "cmd".into() }, PATIENCE)
            .await;
        let Some(ControlResponse::Agents { agents, .. }) = response else {
            panic!("an agents listing was expected: {response:?}");
        };
        let command = agents
            .into_iter()
            .find(|row| row.terminal == terminal)
            .and_then(|row| row.command);
        if command.as_ref().is_some_and(&done) || Instant::now() > deadline {
            return command;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `sleep 1; false` in the tab, read back from its row: status 1, at least a second.
async fn sleep_then_fail(c: &mut TestClient, before: &str) {
    c.input(format!("{before}sleep 1; false\n").as_bytes())
        .await;
    let command = last_command(c, |command| command.exit != Some(0))
        .await
        .expect("no command was reported");
    assert_eq!(command.exit, Some(1), "{command:?}");
    assert!(command.millis >= 1000, "{command:?}");
    assert!(!command.agent, "{command:?}");
}

/// The default: no `ZDOTDIR`. zsh still reads the person's `.zshrc` from their home, the
/// variables used to get there are gone from the shell, and the command is reported.
#[tokio::test]
async fn zsh_reports_a_finished_command_with_its_status_and_time() {
    let _turn = SERIAL.lock().await;
    let _home = home_with(&[(".zshrc", "PROMPT='home> '\n")], None);
    let d = TestDaemon::start_with_shell(Path::new(ZSH)).await;
    let mut c = d.attach("zsh").await;
    assert!(
        c.wait_for_text("home>", PATIENCE).await,
        "the person's .zshrc did not run: {:?}",
        c.screen(c.default_stream()).map(|s| s.lines.clone())
    );
    c.input(b"echo \"zd=${ZDOTDIR-none} qz=${QOOP_ZDOTDIR-none}\"\n")
        .await;
    assert!(
        c.wait_for_text("zd=none qz=none", PATIENCE).await,
        "the injected ZDOTDIR leaked into the shell"
    );
    sleep_then_fail(&mut c, "").await;
}

/// A person's own `ZDOTDIR`: their `.zshrc` there still sets the prompt, `ZDOTDIR` is theirs
/// again inside the shell, and the marks still come.
#[tokio::test]
async fn zsh_with_its_own_zdotdir_keeps_its_prompt_and_still_reports() {
    let _turn = SERIAL.lock().await;
    let zdotdir = tempfile::tempdir().expect("tempdir");
    std::fs::write(zdotdir.path().join(".zshrc"), "PROMPT='custom> '\n").expect("write");
    std::fs::write(zdotdir.path().join(".zshenv"), "export SAW_ZSHENV=yes\n").expect("write");
    let _home = home_with(&[], Some(zdotdir.path()));
    let d = TestDaemon::start_with_shell(Path::new(ZSH)).await;
    let mut c = d.attach("zsh").await;
    assert!(
        c.wait_for_text("custom>", PATIENCE).await,
        "the .zshrc in the person's ZDOTDIR did not run"
    );
    c.input(b"echo \"zd=$ZDOTDIR env=$SAW_ZSHENV\"\n").await;
    let want = format!("zd={} env=yes", zdotdir.path().display());
    assert!(
        c.wait_for_text(&want, PATIENCE).await,
        "ZDOTDIR or the person's .zshenv was not restored: wanted {want}"
    );
    sleep_then_fail(&mut c, "").await;
}

/// bash 4.4 or newer through `--rcfile`: the person's `.bashrc` runs, their own
/// `PROMPT_COMMAND` still sees the command's status, and the command is reported.
#[tokio::test]
async fn new_bash_reports_a_finished_command_and_keeps_the_persons_prompt_command() {
    // The turn first: the probe starts a process, which reads the environment another test
    // may be writing.
    let _turn = SERIAL.lock().await;
    if !bash_has_ps0(Path::new(NEW_BASH)) {
        qoop_runtime::say!("skipped: no bash 4.4 or newer at {NEW_BASH}");
        return;
    }
    let _home = home_with(
        &[(
            ".bashrc",
            "PS1='bash> '\nPROMPT_COMMAND='echo \"pc-saw-$?\"'\n",
        )],
        None,
    );
    let d = TestDaemon::start_with_shell(Path::new(NEW_BASH)).await;
    let mut c = d.attach("bash").await;
    assert!(
        c.wait_for_text("bash>", PATIENCE).await,
        "the person's .bashrc did not run"
    );
    sleep_then_fail(&mut c, "").await;
    assert!(
        c.wait_for_text("pc-saw-1", PATIENCE).await,
        "the person's PROMPT_COMMAND lost the command's status"
    );
}

/// A mark longer than the 4 KiB a control string may be is dropped whole, as every
/// oversized one is: it neither ends the command nor stops the real marks that follow.
#[tokio::test]
async fn an_oversized_mark_is_dropped_and_the_real_ones_still_count() {
    let _turn = SERIAL.lock().await;
    let _home = home_with(&[(".zshrc", "PROMPT='home> '\n")], None);
    let d = TestDaemon::start_with_shell(Path::new(ZSH)).await;
    let mut c = d.attach("zsh").await;
    assert!(c.wait_for_text("home>", PATIENCE).await, "no prompt");
    // A D with status 7 padded past 4 KiB, printed while the command runs. Taken, it would
    // end the command at once with 7 and leave the real D nothing to end.
    sleep_then_fail(&mut c, "printf '\\033]133;D;7%4200s\\007' x; ").await;
}

fn bash_has_ps0(bash: &Path) -> bool {
    let version = std::process::Command::new(bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"])
        .output();
    let Ok(out) = version else { return false };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text
        .split_whitespace()
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (major, minor) >= (4, 4)
}

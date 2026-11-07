//! A tab's row keeps its agent for a while after the agent ends, so a shutdown that ends the
//! agents before the daemon does not bring their tabs back as bare shells.
//!
//! A reboot, a logout or a killed daemon sets no stopping flag: the agents get their signal
//! and often go first. Measured on a macOS 27.2 restart: seven tabs whose rows named Claude
//! and its conversation were cleared in that gap and came back as shells. A person who quits
//! an agent is still written down as having quit, once the grace is over.
//!
//! A stand in named `claude`, compiled rather than a shell script, because the daemon reads
//! the program's name off the process.

use std::path::Path;
use std::time::{Duration, Instant};

use qoop_testkit::TestDaemon;

/// Build the stand in: it writes its pid to the file named by its first argument and waits.
fn stand_in(dir: &Path) -> std::path::PathBuf {
    let source = dir.join("claude.rs");
    let program = r#"fn main() {
    let at = std::env::args().nth(1).unwrap();
    std::fs::write(at, std::process::id().to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
}
"#;
    std::fs::write(&source, program).expect("write");
    let built = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(dir.join("claude"))
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    dir.join("claude")
}

const SESSION: &str = "3f1c2b4a-5d6e-4f70-8a9b-0c1d2e3f4a5b";

/// The row's agent and its conversation id.
async fn row(d: &TestDaemon, tab: u32) -> (Option<String>, Option<String>) {
    d.store()
        .open_terminals(d.workspace())
        .await
        .expect("rows")
        .into_iter()
        .find(|row| row.id == tab as i64)
        .map(|row| (row.agent, row.agent_session))
        .unwrap_or_default()
}

async fn row_agent(d: &TestDaemon, tab: u32) -> Option<String> {
    row(d, tab).await.0
}

/// What the agent's own hook says: which conversation it is having.
async fn report_session(d: &TestDaemon, c: &mut qoop_testkit::TestClient, tab: u32) {
    let token = d
        .terminals()
        .get(tab)
        .expect("the tab")
        .token
        .as_str()
        .to_string();
    let answer = c
        .control(
            &qoop_runtime::control::ControlRequest::ReportAgentState {
                from: None,
                id: "hook".into(),
                terminal: tab,
                token,
                state: "idle".into(),
                visible_blocker: false,
                visible_idle: true,
                visible_working: false,
                clock: 0,
                session: Some(SESSION.into()),
                event: None,
                label: None,
                said: None,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(
            answer,
            Some(qoop_runtime::control::ControlResponse::AgentStateReported { .. })
        ),
        "{answer:?}"
    );
}

fn is_claude(d: &TestDaemon, tab: u32) -> bool {
    d.agent(tab).is_some_and(|a| {
        matches!(&a.identity, qoop_domain::AgentIdentity::Known { program, .. } if program == "claude")
    })
}

/// Start the stand in in the tab and wait until it is read, and written down, as claude.
async fn start(
    d: &TestDaemon,
    c: &mut qoop_testkit::TestClient,
    tab: u32,
    claude: &Path,
    pid_file: &Path,
) -> i32 {
    let _ = std::fs::remove_file(pid_file);
    c.input_to(
        tab,
        format!("'{}' '{}'\n", claude.display(), pid_file.display()).as_bytes(),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(10);
    // The pid file too: the daemon can read the name before the stand in has written it.
    while Instant::now() < deadline
        && !(is_claude(d, tab)
            && pid_file.exists()
            && row_agent(d, tab).await.as_deref() == Some("claude"))
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        row_agent(d, tab).await.as_deref(),
        Some("claude"),
        "the stand in was never written down as claude: {:?}",
        d.agent(tab)
    );
    std::fs::read_to_string(pid_file)
        .expect("the stand in's pid")
        .parse()
        .expect("a pid")
}

/// What a shutdown does to the agent, ahead of the daemon; waits until it is seen gone.
async fn end(d: &TestDaemon, tab: u32, pid: i32) {
    // SAFETY: kill with a pid this test started and a valid signal.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && is_claude(d, tab) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!is_claude(d, tab), "the agent was never seen to go");
}

#[tokio::test]
async fn an_agent_that_ends_stays_in_its_row_through_the_grace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = stand_in(dir.path());
    let pid_file = dir.path().join("pid");
    // A transcript for the conversation, so its id is one a resume can carry on.
    let config = dir.path().join("config");
    let project = config.join("projects/-tmp");
    std::fs::create_dir_all(&project).expect("projects");
    std::fs::write(project.join(format!("{SESSION}.jsonl")), "{}\n").expect("transcript");
    // SAFETY: the only test in this binary, before its daemon exists; nothing else writes
    // the environment.
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &config) };
    let d = TestDaemon::start().await;
    let tab = d.terminals().streams()[0];
    let mut c = d.attach("shutdown").await;

    let pid = start(&d, &mut c, tab, &claude, &pid_file).await;
    report_session(&d, &mut c, tab).await;
    assert_eq!(row(&d, tab).await.1.as_deref(), Some(SESSION));
    end(&d, tab, pid).await;

    // Ample time for the write the grace holds back; a daemon dying now leaves the agent.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        row(&d, tab).await,
        (Some("claude".into()), Some(SESSION.into())),
        "the row forgot its agent or its conversation at once, so a shutdown that ends \
         the agents first brings the tab back as a bare shell or a fresh agent"
    );

    // An agent started again within the grace is a new one: the old conversation is not its.
    let pid = start(&d, &mut c, tab, &claude, &pid_file).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        row(&d, tab).await,
        (Some("claude".into()), None),
        "an agent started again took the conversation of the one before, so a restart \
         resumes a conversation it is not having"
    );

    // And once the grace is over, an agent that stays gone is written down as gone.
    report_session(&d, &mut c, tab).await;
    end(&d, tab, pid).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && row_agent(&d, tab).await.is_some() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        row(&d, tab).await,
        (None, None),
        "an agent that stayed gone past the grace is still in its row, so a person who \
         quit it gets it back on the next start"
    );
}

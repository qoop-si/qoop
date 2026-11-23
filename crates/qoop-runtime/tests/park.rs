//! `SetParked`: an agent sets itself aside from inside its own turn (`qoop park`) and reads
//! parked once the turn ends; a terminal that is no agent is refused; a park never hides
//! what waits on a person.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse, TaskOp};
use qoop_testkit::{TestClient, TestDaemon};

const PATIENCE: Duration = Duration::from_secs(5);

async fn row(asker: &mut TestClient, terminal: u32) -> AgentRow {
    // Boxed: a control call holds a 64 KiB read buffer, see news.rs.
    match Box::pin(asker.control(&ControlRequest::ListAgents { id: "rows".into() }, PATIENCE)).await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .find(|row| row.terminal == terminal)
            .expect("a row for the terminal"),
        other => panic!("no index: {other:?}"),
    }
}

async fn status(asker: &mut TestClient, terminal: u32) -> String {
    row(asker, terminal).await.status.unwrap_or_default()
}

async fn report(hook: &mut TestClient, terminal: u32, token: &str, state: &str, event: &str) {
    let answer = Box::pin(hook.control(
        &ControlRequest::ReportAgentState {
            from: None,
            id: format!("{state}-{event}"),
            terminal,
            token: token.into(),
            state: state.into(),
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            clock: 0,
            session: None,
            event: Some(event.into()),
            label: None,
            said: None,
        },
        PATIENCE,
    ))
    .await;
    assert!(
        !matches!(answer, Some(ControlResponse::Failed { .. }) | None),
        "the report was refused: {answer:?}"
    );
}

async fn park(c: &mut TestClient, terminal: u32, on: bool) -> ControlResponse {
    Box::pin(c.control(
        &ControlRequest::SetParked {
            id: format!("park-{on}"),
            terminal,
            on,
        },
        PATIENCE,
    ))
    .await
    .expect("an answer")
}

/// A program named `claude` that draws nothing and waits: the daemon reads the terminal as an
/// agent from the process alone, and no screen evidence disagrees with what a hook says.
fn stand_in(dir: &Path) -> PathBuf {
    let source = dir.join("stand_in.rs");
    std::fs::write(
        &source,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(120)); }",
    )
    .expect("write");
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

/// Run the stand in in the daemon's first tab and wait until it reads as an agent.
async fn become_agent(d: &TestDaemon, claude: &Path) -> u32 {
    let mut c = d.attach("park-agent").await;
    let tab = d.terminals().streams()[0];
    c.input_to(tab, format!("'{}'\n", claude.display()).as_bytes())
        .await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if d.agent(tab).is_some_and(|a| {
            matches!(
                &a.identity,
                qoop_domain::AgentIdentity::Known { agent: true, .. }
            )
        }) {
            return tab;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "the stand in was never read as an agent: {:?}",
        d.agent(tab)
    );
}

fn token_of(d: &TestDaemon, terminal: u32) -> String {
    d.terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string()
}

#[tokio::test]
async fn a_terminal_that_is_no_agent_is_not_parked_and_can_always_be_taken_back() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut asker = d.attach("qoop-agents").await;

    match park(&mut asker, terminal, true).await {
        ControlResponse::Failed { reason, .. } => {
            assert!(reason.contains("only an agent"), "{reason}");
        }
        other => panic!("a shell was parked: {other:?}"),
    }
    assert!(!row(&mut asker, terminal).await.parked);
    // Taking a park back is never refused: it clears one a gone agent left behind.
    assert!(matches!(
        park(&mut asker, terminal, false).await,
        ControlResponse::Renamed { .. }
    ));
}

#[tokio::test]
async fn an_agent_parks_itself_mid_turn_and_reads_parked_once_the_turn_ends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = stand_in(dir.path());
    let d = TestDaemon::start().await;
    let terminal = become_agent(&d, &claude).await;
    let token = token_of(&d, terminal);
    let mut hook = d.attach("hook").await;
    let mut asker = d.attach("qoop-agents").await;

    report(&mut hook, terminal, &token, "working", "user").await;
    // The agent runs `qoop park` inside its own turn: accepted, and still work to look at.
    assert!(matches!(
        park(&mut asker, terminal, true).await,
        ControlResponse::Renamed { .. }
    ));
    assert_eq!(status(&mut asker, terminal).await, "working");
    report(&mut hook, terminal, &token, "idle", "done").await;
    assert_eq!(status(&mut asker, terminal).await, "parked");

    // Taken back, the finish nobody looked at is news again.
    assert!(matches!(
        park(&mut asker, terminal, false).await,
        ControlResponse::Renamed { .. }
    ));
    assert_eq!(status(&mut asker, terminal).await, "done");
}

#[tokio::test]
async fn a_park_never_hides_a_task_waiting_on_the_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = stand_in(dir.path());
    let d = TestDaemon::start().await;
    let terminal = become_agent(&d, &claude).await;
    let token = token_of(&d, terminal);
    let mut hook = d.attach("hook").await;
    let mut asker = d.attach("qoop-agents").await;
    report(&mut hook, terminal, &token, "working", "user").await;
    report(&mut hook, terminal, &token, "idle", "done").await;
    let write = |id: &str, action| ControlRequest::TaskWrite {
        id: id.into(),
        terminal: Some(terminal),
        token: Some(token.clone()),
        task: None,
        action,
    };
    let began = hook.control(&write("new", TaskOp::Begin), PATIENCE).await;
    assert!(matches!(began, Some(ControlResponse::TaskWritten { .. })));
    let stage = TaskOp::Stage {
        stage: "waiting-human".into(),
    };
    let wrote = hook.control(&write("wait", stage), PATIENCE).await;
    assert!(matches!(wrote, Some(ControlResponse::TaskWritten { .. })));

    assert!(matches!(
        park(&mut asker, terminal, true).await,
        ControlResponse::Renamed { .. }
    ));
    assert_eq!(status(&mut asker, terminal).await, "blocked");
}

//! A tab whose Claude is in Claude Code's agent view follows the job it has open.
//!
//! In the agent view the `claude` in the tab parks its own conversation and shows a job that
//! runs under Claude's own daemon, whose hooks cannot name the tab. Measured on a real tab: the
//! tab kept its host's never used id, so the phone showed a number and "nothing here".
//!
//! A stand in named `claude`, compiled rather than a shell script, for two reasons: the
//! daemon reads the program's name off the process, and macOS hides the environment (and so
//! `CLAUDE_CONFIG_DIR`) of its own platform binaries such as `/bin/sh`. It lays out a Claude
//! config directory the way Claude Code 2.1.309 does and draws the job's prompt box.

use std::path::Path;
use std::time::{Duration, Instant};

use qoop_testkit::TestDaemon;

const JOB: &str = "5a1e0b2c-7d3f-4e8a-9b6c-0d1e2f3a4b5c";
const NAME: &str = "Café build server setup";

/// Build the stand in: it writes its own pid file under `CLAUDE_CONFIG_DIR` (a host whose
/// conversation went to the background when its first argument is `parked`), then draws the
/// prompt box of the job it shows and waits.
fn stand_in(dir: &Path) -> std::path::PathBuf {
    let source = dir.join("claude.rs");
    let program = r##"fn main() {
    let config = std::env::var("CLAUDE_CONFIG_DIR").unwrap();
    let pid = std::process::id();
    let parked = if std::env::args().nth(1).as_deref() == Some("parked") {
        ",\"parkedJobId\":\"7b3e9a10\""
    } else {
        ""
    };
    let file = format!(
        "{{\"pid\":{pid},\"sessionId\":\"0c9d8e7f-6a5b-4c3d-8e2f-1a0b9c8d7e6f\",\"kind\":\"interactive\"{parked}}}"
    );
    std::fs::write(format!("{config}/sessions/{pid}.json"), file).unwrap();
    println!("Waiting on your answer");
    println!("───── NAME ─");
    println!("❯ ");
    println!("──────");
    std::thread::sleep(std::time::Duration::from_secs(60));
}
"##;
    std::fs::write(&source, program.replace("NAME", NAME)).expect("write");
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

/// A Claude config directory with one live worker running the job, and its transcript.
fn layout(config: &Path) {
    std::fs::create_dir_all(config.join("sessions")).expect("sessions");
    let worker = serde_json::json!({
        "pid": std::process::id(), "sessionId": JOB, "kind": "bg", "jobId": &JOB[..8],
        "name": NAME, "status": "waiting",
    });
    std::fs::write(config.join("sessions/61078.json"), worker.to_string()).expect("worker");
    let project = config.join("projects/-Users-me-coding-app");
    std::fs::create_dir_all(&project).expect("projects");
    std::fs::write(
        project.join(format!("{JOB}.jsonl")),
        format!("{{\"type\":\"ai-title\",\"aiTitle\":\"{NAME}\",\"sessionId\":\"{JOB}\"}}\n"),
    )
    .expect("transcript");
}

/// Start the stand in in the daemon's first tab and wait for it to be read as Claude with
/// its prompt box drawn.
async fn run(d: &TestDaemon, claude: &Path, config: &Path, arg: &str) -> u32 {
    let mut c = d.attach("agent-view").await;
    let tab = d.terminals().streams()[0];
    c.input_to(
        tab,
        format!(
            "CLAUDE_CONFIG_DIR='{}' '{}' {arg}\n",
            config.display(),
            claude.display()
        )
        .as_bytes(),
    )
    .await;
    assert!(
        c.wait_for_text_on(tab, NAME, Duration::from_secs(10)).await,
        "the stand in never drew its prompt box"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if d.agent(tab).is_some_and(|a| {
            matches!(&a.identity, qoop_domain::AgentIdentity::Known { program, .. } if program == "claude")
        }) {
            return tab;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the stand in was never read as claude: {:?}", d.agent(tab));
}

#[tokio::test]
async fn a_tab_in_the_agent_view_follows_the_job_it_has_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = stand_in(dir.path());
    let config = dir.path().join("config");
    layout(&config);
    let d = TestDaemon::start().await;
    let tab = run(&d, &claude, &config, "parked").await;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && d.terminals().session_of(tab).as_deref() != Some(JOB) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let session = d.terminals().session_of(tab);
    assert_eq!(
        session.as_deref(),
        Some(JOB),
        "the tab kept the host's own conversation"
    );
    // What the phone and `qoop history` read for it: the job's transcript.
    let transcript = qoop_runtime::daemon::claude_transcript_in(&[config.join("projects")], JOB);
    assert!(transcript.is_some(), "the job's transcript is not found");
}

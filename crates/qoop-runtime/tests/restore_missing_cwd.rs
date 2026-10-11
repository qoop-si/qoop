//! A tab whose agent's folder is gone is never resumed anywhere else.
//!
//! An agent keeps its conversation under the folder it ran in, and asks to trust the folder
//! it is started in. A restart that found the folder missing used to open the shell where
//! the daemon is (the home folder) and type the resume there, so the person was asked to
//! trust a folder they never chose. These gates run the real daemon twice over one
//! directory, with a resume that touches a marker file: whether it ran is a fact on disk,
//! where the echoed command on the screen would read the same with and without the fix.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qoop_runtime::{Daemon, paths};

/// Held for the whole test: the override table is process wide.
static OVERRIDES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn runtime_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    paths::ensure_private_dir(dir.path()).expect("a private directory");
    dir
}

async fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    done()
}

/// An agent that resumes by touching its own marker, so a resume that ran leaves a file.
fn install_agent(overrides: &Path, id: &str, marker: &Path) {
    std::fs::write(
        overrides.join(format!("{id}.toml")),
        format!(
            "id = \"{id}\"\nversion = \"test\"\n[resume]\nlatest = [\"/usr/bin/touch\", \"{}\"]\n",
            marker.display()
        ),
    )
    .expect("an override");
}

struct Ground {
    /// Where the tabs' shells record their folders and the resumes their markers.
    marks: PathBuf,
    /// A shell that records where it started and every line typed into it, per tab, and runs
    /// what it is typed.
    shell: PathBuf,
}

fn ground(base: &Path) -> Ground {
    let marks = base.join("marks");
    std::fs::create_dir(&marks).expect("mkdir");
    let shell = base.join("recording-shell");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\npwd -P > \"{0}/$QOOP_TERMINAL.pwd\"\ntee \"{0}/$QOOP_TERMINAL.typed\" | /bin/sh\n",
            marks.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    Ground { marks, shell }
}

impl Ground {
    fn pwd(&self, tab: u32) -> Option<PathBuf> {
        std::fs::read_to_string(self.marks.join(format!("{tab}.pwd")))
            .ok()
            .map(|s| PathBuf::from(s.trim()))
    }

    /// Everything typed into the tab so far.
    fn typed(&self, tab: u32) -> String {
        std::fs::read_to_string(self.marks.join(format!("{tab}.typed"))).unwrap_or_default()
    }

    fn resumed(&self, agent: &str) -> bool {
        self.marks.join(format!("{agent}.resumed")).exists()
    }

    fn forget_all(&self) {
        for entry in std::fs::read_dir(&self.marks).expect("read").flatten() {
            std::fs::remove_file(entry.path()).expect("remove a mark");
        }
    }
}

#[tokio::test]
async fn an_agent_whose_folder_is_gone_is_not_resumed_in_the_daemons_folder() {
    let _overrides = OVERRIDES.lock().await;
    let dir = runtime_dir();
    let held = tempfile::tempdir().expect("dir");
    let base = std::fs::canonicalize(held.path()).expect("real");
    let ground = ground(&base);

    let overrides = base.join("overrides");
    std::fs::create_dir(&overrides).expect("mkdir");
    for agent in ["gone-a-xyz", "fine-b-xyz", "relative-g-xyz"] {
        install_agent(
            &overrides,
            agent,
            &ground.marks.join(format!("{agent}.resumed")),
        );
    }
    let outcome = qoop_detect::reload_overrides(&overrides);
    assert!(outcome.errors.is_empty(), "{outcome:?}");

    // A folder that is gone under one that is there, as a deleted worktree is.
    let projects = base.join("projects");
    std::fs::create_dir(&projects).expect("mkdir");
    let gone = projects.join("worktree-that-was-deleted");
    let fine = base.join("fine");
    std::fs::create_dir(&fine).expect("mkdir");
    let plain_gone = base.join("plain-gone");

    let (a, b, d, g) = {
        let _binding = qoop_testkit::BINDING.lock().await;
        let daemon = Daemon::bind(dir.path(), Path::new("/bin/sh"))
            .await
            .expect("the daemon binds");
        let (terminals, store, workspace) =
            (daemon.terminals(), daemon.store(), daemon.workspace());
        let row = async |cwd: &Path, agent: Option<(&str, &str)>| {
            let id = terminals.mint();
            store
                .create_terminal(
                    id as i64,
                    workspace,
                    cwd.display().to_string(),
                    "/bin/sh".into(),
                )
                .await
                .expect("a row");
            if let Some((agent, session)) = agent {
                store
                    .set_agent(id as i64, Some(agent.into()), Some(session.into()))
                    .await
                    .expect("an agent");
            }
            id
        };
        (
            row(&gone, Some(("gone-a-xyz", "session-a"))).await,
            row(&fine, Some(("fine-b-xyz", "session-b"))).await,
            row(&plain_gone, None).await,
            row(
                Path::new("relative/dir"),
                Some(("relative-g-xyz", "session-g")),
            )
            .await,
        )
    };

    let _binding = qoop_testkit::BINDING.lock().await;
    let daemon = Daemon::bind(dir.path(), &ground.shell)
        .await
        .expect("the daemon binds");
    let report = daemon.restore_report();

    // The control: an agent whose folder is there is resumed, so the absences below are
    // not just a resume that never works in this harness.
    assert!(
        until(|| ground.resumed("fine-b-xyz")).await,
        "the agent with its folder was not resumed: {report:?}"
    );
    assert!(until(|| [a, d, g].iter().all(|t| ground.pwd(*t).is_some())).await);
    // The folder-gone tab starts in the nearest folder that exists, not where the daemon is.
    assert_eq!(ground.pwd(a), Some(projects.clone()));
    assert_eq!(ground.pwd(b), Some(fine.clone()));
    // A plain shell keeps the old fallback.
    let here = std::fs::canonicalize(std::env::current_dir().expect("cwd")).expect("real");
    assert_eq!(ground.pwd(d), Some(here));
    // And the tab is told so, with a command that can be pasted.
    assert!(
        until(|| ground.typed(a).contains("agent folder gone")).await,
        "{}",
        ground.typed(a)
    );
    assert!(
        until(|| ground.typed(g).contains("agent folder not available")).await,
        "{}",
        ground.typed(g)
    );
    // Everything else typed by the restore had time to run by now.
    assert!(!ground.resumed("gone-a-xyz"), "resumed in the wrong folder");
    assert!(
        !ground.resumed("relative-g-xyz"),
        "resumed in the wrong folder"
    );

    assert_eq!(report.brought_back, 4, "{report:?}");
    assert_eq!(report.agents_resumed, 1, "{report:?}");
    assert_eq!(report.agents_folder_gone, 2, "{report:?}");
    assert_eq!(report.agents_shell_fallback, 0, "{report:?}");
    assert_eq!(report.plain_shells, 1, "{report:?}");

    // The row forgot the agent but kept the conversation's id; the others are untouched.
    let rows = daemon
        .store()
        .open_terminals(daemon.workspace())
        .await
        .expect("rows");
    let row = |id: u32| rows.iter().find(|r| r.id == id as i64).expect("a row");
    assert_eq!(row(a).agent, None);
    assert_eq!(row(a).agent_session.as_deref(), Some("session-a"));
    assert_eq!(row(b).agent.as_deref(), Some("fine-b-xyz"));
    assert_eq!(row(g).agent, None);

    // The person went to the home folder and stopped: the row now names a folder that is
    // there, and without the forgotten agent the next start would resume in it.
    daemon
        .store()
        .set_cwd(a as i64, fine.display().to_string())
        .await
        .expect("a cwd");
    drop(daemon);
    ground.forget_all();
    let daemon = Daemon::bind(dir.path(), &ground.shell)
        .await
        .expect("the daemon binds again");
    assert!(until(|| ground.resumed("fine-b-xyz")).await);
    assert!(until(|| ground.pwd(a).is_some()).await);
    assert!(
        !ground.resumed("gone-a-xyz"),
        "the next start resumed a conversation whose folder was gone"
    );
    drop(daemon);
}

//! A space has a home: a new tab that names no directory starts there, never where another
//! tab of the space moved to. Driven through the socket against real shells.

use std::path::Path;

use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE)
        .await
        .expect("an answer to the request")
}

async fn new_tab(c: &mut TestClient, workspace: i64, id: &str, cwd: Option<&Path>) -> u32 {
    match ask(
        c,
        ControlRequest::CreateTerminal {
            id: id.into(),
            cwd: cwd.map(|p| p.display().to_string()),
            workspace: Some(workspace),
            name: None,
        },
    )
    .await
    {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        other => panic!("expected a created terminal, got {other:?}"),
    }
}

async fn home_of(c: &mut TestClient, workspace: i64) -> Option<String> {
    match ask(c, ControlRequest::ListWorkspaces { id: "list".into() }).await {
        ControlResponse::Workspaces { workspaces, .. } => workspaces
            .into_iter()
            .find(|w| w.id == workspace)
            .and_then(|w| w.dir),
        other => panic!("expected the spaces, got {other:?}"),
    }
}

/// Asked of the shell itself: a temporary directory on macOS is reached through a symlink,
/// so its own `pwd` is the answer that can be checked.
async fn starts_in(c: &mut TestClient, terminal: u32, dir: &Path) -> bool {
    c.input_to(terminal, b"pwd\n").await;
    let real = std::fs::canonicalize(dir).expect("canonicalize");
    let name = real.file_name().expect("a name").to_string_lossy();
    c.wait_for_text_on(terminal, &name, PATIENCE).await
}

/// The bug: a `+` in a space whose tab sat in a worktree opened in that worktree. A space with
/// no home learns one at its first tab asked for with no directory, from where its tabs were,
/// a worktree's being its main checkout; a tab elsewhere afterwards does not move it.
#[tokio::test]
async fn a_new_tab_starts_in_its_space_home_not_where_a_tab_moved() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("space-dir").await;
    let main = d.dir().join("homebase");
    std::fs::create_dir_all(main.join(".git/worktrees/branchy")).expect("main");
    let worktree = d.dir().join("branchy");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::write(
        worktree.join(".git"),
        format!(
            "gitdir: {}\n",
            main.join(".git/worktrees/branchy").display()
        ),
    )
    .expect("dot git");
    let elsewhere = d.dir().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("elsewhere");

    let space = match ask(
        &mut c,
        ControlRequest::CreateWorkspace {
            id: "space".into(),
            name: "work".into(),
        },
    )
    .await
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a space, got {other:?}"),
    };
    new_tab(&mut c, space, "first", Some(&worktree)).await;
    assert_eq!(
        home_of(&mut c, space).await,
        None,
        "a tab placed by hand never makes a home"
    );
    let plus = new_tab(&mut c, space, "plus", None).await;
    assert_eq!(
        home_of(&mut c, space).await.as_deref(),
        Some(main.display().to_string().as_str()),
        "learned from its tab, a worktree's being its main checkout"
    );
    assert!(
        starts_in(&mut c, plus, &main).await,
        "a tab naming no directory starts at home"
    );
    new_tab(&mut c, space, "elsewhere", Some(&elsewhere)).await;
    let again = new_tab(&mut c, space, "again", None).await;
    assert!(starts_in(&mut c, again, &main).await, "home stays put");
}

/// `SetSpaceDir` moves the home, refuses what is not a directory, and a home gone since
/// opens the tab where the daemon is rather than failing it.
#[tokio::test]
async fn a_space_home_is_set_checked_and_survives_its_folder_going() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("space-dir-set").await;
    let space = match ask(
        &mut c,
        ControlRequest::CreateWorkspace {
            id: "space".into(),
            name: "w".into(),
        },
    )
    .await
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a space, got {other:?}"),
    };
    let set = |id: &str, dir: String| ControlRequest::SetSpaceDir {
        id: id.into(),
        workspace: space,
        dir,
    };
    for (id, dir) in [
        ("rel", "not/absolute".to_string()),
        ("missing", d.dir().join("nope").display().to_string()),
    ] {
        let answer = ask(&mut c, set(id, dir)).await;
        assert!(
            matches!(answer, ControlResponse::Failed { .. }),
            "{answer:?}"
        );
    }
    let picked = d.dir().join("picked");
    std::fs::create_dir(&picked).expect("picked");
    let answer = ask(&mut c, set("ok", format!("{}/", picked.display()))).await;
    assert!(
        matches!(answer, ControlResponse::Renamed { .. }),
        "{answer:?}"
    );
    assert_eq!(
        home_of(&mut c, space).await.as_deref(),
        Some(picked.display().to_string().as_str()),
        "kept without its trailing slash"
    );
    let tab = new_tab(&mut c, space, "at-home", None).await;
    assert!(starts_in(&mut c, tab, &picked).await);
    std::fs::remove_dir(&picked).expect("gone");
    new_tab(&mut c, space, "home-gone", None).await;
}

//! A space that sleeps ends everything in it, keeps its tabs, and comes back as it was.
//!
//! Driven through the socket against real shells: the refusals that keep a sleep from
//! losing work, the round trip that has to bring the same tabs back, and the paths that
//! must not leave a space half awake.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse, Pane, SplitDir, WorkspaceRow};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE)
        .await
        .expect("an answer to the request")
}

async fn new_space(c: &mut TestClient, name: &str) -> i64 {
    match ask(
        c,
        ControlRequest::CreateWorkspace {
            id: format!("space-{name}"),
            name: name.into(),
        },
    )
    .await
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a created workspace, got {other:?}"),
    }
}

async fn new_tab(c: &mut TestClient, workspace: i64, id: &str) -> u32 {
    match ask(
        c,
        ControlRequest::CreateTerminal {
            id: id.into(),
            cwd: None,
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

async fn spaces(c: &mut TestClient) -> Vec<WorkspaceRow> {
    match ask(c, ControlRequest::ListWorkspaces { id: "list".into() }).await {
        ControlResponse::Workspaces { workspaces, .. } => workspaces,
        other => panic!("expected the spaces, got {other:?}"),
    }
}

/// Ask for a sleep until the shells have settled past starting, and answer what the last
/// ask said. A fresh shell is refused while it starts, which is right and is not what these
/// tests are about.
async fn sleep(c: &mut TestClient, workspace: i64) -> ControlResponse {
    Box::pin(sleep_tab(c, workspace, None)).await
}

/// The same, for one tab (`Some`) or every tab of the space (`None`).
async fn sleep_tab(c: &mut TestClient, workspace: i64, terminal: Option<u32>) -> ControlResponse {
    let deadline = Instant::now() + PATIENCE;
    let mut n = 0;
    loop {
        n += 1;
        let answer = ask(
            c,
            ControlRequest::SleepWorkspace {
                id: format!("sleep-{workspace}-{n}"),
                workspace,
                terminal,
            },
        )
        .await;
        let starting = matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains("starting") || reason.contains("not been read"));
        if !starting || Instant::now() > deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wake(c: &mut TestClient, workspace: i64) -> ControlResponse {
    Box::pin(wake_tab(c, workspace, None)).await
}

async fn wake_tab(c: &mut TestClient, workspace: i64, terminal: Option<u32>) -> ControlResponse {
    ask(
        c,
        ControlRequest::WakeWorkspace {
            id: format!("wake-{workspace}-{terminal:?}"),
            workspace: Some(workspace),
            terminal,
        },
    )
    .await
}

#[tokio::test]
async fn a_space_sleeps_and_wakes_with_the_same_tab_and_its_last_screen() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("sleeper").await;
    let space = new_space(&mut c, "resting").await;
    let tab = new_tab(&mut c, space, "t1").await;
    c.input_to(tab, b"echo SLEEPMARK-$((6*7))\n").await;
    assert!(
        c.wait_for_text_on(tab, "SLEEPMARK-42", PATIENCE).await,
        "the marker never reached the screen"
    );

    let asleep = sleep(&mut c, space).await;
    assert!(
        matches!(
            asleep,
            ControlResponse::WorkspaceAsleep { terminals: 1, .. }
        ),
        "the space did not sleep: {asleep:?}"
    );
    assert!(
        d.terminals().get(tab).is_none(),
        "a tab of a sleeping space is still running"
    );
    let listed = spaces(&mut c).await;
    let row = listed.iter().find(|s| s.id == space).expect("the space");
    assert!(row.asleep, "the list says the space is awake: {row:?}");
    assert_eq!(
        row.sleeping.iter().map(|t| t.terminal).collect::<Vec<_>>(),
        vec![tab],
        "the list does not name the sleeping tab"
    );

    // Asleep, its screen can still be read, so a client can show it before waking it.
    let peeked = ask(
        &mut c,
        ControlRequest::Scrollback {
            id: "peek".into(),
            terminal: tab,
            up: 50,
            rows: 50,
        },
    )
    .await;
    let ControlResponse::Scrollback { lines, .. } = peeked else {
        panic!("a sleeping tab's screen was not readable: {peeked:?}");
    };
    assert!(
        lines.iter().any(|l| l.trim() == "SLEEPMARK-42"),
        "the sleeping tab's last screen is not readable: {lines:?}"
    );
    // A number no sleeping space holds is still refused, and leaves no directory behind.
    let stranger = ask(
        &mut c,
        ControlRequest::Scrollback {
            id: "stranger".into(),
            terminal: tab + 500,
            up: 50,
            rows: 50,
        },
    )
    .await;
    assert!(
        matches!(stranger, ControlResponse::Failed { .. }),
        "{stranger:?}"
    );

    let woke = wake(&mut c, space).await;
    assert!(
        matches!(woke, ControlResponse::WorkspaceAwake { terminals: 1, .. }),
        "the space did not wake: {woke:?}"
    );
    assert!(
        d.terminals().get(tab).is_some(),
        "the tab came back under another number"
    );
    let back = ask(
        &mut c,
        ControlRequest::Scrollback {
            id: "back".into(),
            terminal: tab,
            up: 200,
            rows: 200,
        },
    )
    .await;
    let ControlResponse::Scrollback { lines, .. } = back else {
        panic!("no scrollback: {back:?}");
    };
    assert!(
        lines.iter().any(|l| l.trim() == "SLEEPMARK-42"),
        "the last screen before the sleep is not in the history: {lines:?}"
    );
    // The woken tab renders for the same connection that watched it before the sleep: the
    // number is the same, and the subscription made for its earlier life must not stand in
    // for one to the new.
    let subscribed = ask(
        &mut c,
        ControlRequest::Subscribe {
            id: "again".into(),
            terminals: vec![tab],
        },
    )
    .await;
    assert!(
        matches!(&subscribed, ControlResponse::Subscribed { terminals, .. } if terminals == &vec![tab]),
        "{subscribed:?}"
    );
    c.input_to(tab, b"echo AWAKE-$((2+2))\n").await;
    assert!(
        c.wait_for_text_on(tab, "AWAKE-4", PATIENCE).await,
        "the woken tab does not render to a connection that watched it before"
    );
    let row = spaces(&mut c).await.into_iter().find(|s| s.id == space);
    assert!(
        row.is_some_and(|r| !r.asleep),
        "the space still reads as asleep"
    );
}

#[tokio::test]
async fn a_command_running_in_front_keeps_the_space_awake() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("busy").await;
    let space = new_space(&mut c, "busy").await;
    let tab = new_tab(&mut c, space, "t1").await;
    c.input_to(tab, b"sleep 30\n").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let answer = sleep(&mut c, space).await;
    assert!(
        matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains("is running in it")),
        "a running command was put to sleep: {answer:?}"
    );
    assert!(d.terminals().get(tab).is_some(), "the refused tab ended");
}

#[tokio::test]
async fn a_job_in_the_background_keeps_the_space_awake() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("jobs").await;
    let space = new_space(&mut c, "jobs").await;
    let tab = new_tab(&mut c, space, "t1").await;
    // `sh` has no job control when not interactive in every build; `set -m` asks for it,
    // so the job gets a group of its own as it would at a person's prompt.
    c.input_to(tab, b"set -m; sleep 300 & echo JOB-$((1+1))\n")
        .await;
    assert!(c.wait_for_text_on(tab, "JOB-2", PATIENCE).await);
    let answer = sleep(&mut c, space).await;
    assert!(
        matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains("background")),
        "a background job was put to sleep: {answer:?}"
    );
    c.input_to(tab, b"kill %1\n").await;
}

#[tokio::test]
async fn a_sleeping_tab_closes_for_good_without_waking_its_space() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("closer").await;
    let space = new_space(&mut c, "closing").await;
    let first = new_tab(&mut c, space, "t1").await;
    let second = new_tab(&mut c, space, "t2").await;
    let asleep = sleep(&mut c, space).await;
    assert!(
        matches!(asleep, ControlResponse::WorkspaceAsleep { .. }),
        "{asleep:?}"
    );

    // Not while it holds tabs.
    let refused = ask(
        &mut c,
        ControlRequest::CloseWorkspace {
            id: "cw1".into(),
            workspace: space,
        },
    )
    .await;
    assert!(
        matches!(refused, ControlResponse::Failed { .. }),
        "a sleeping space with tabs was deleted: {refused:?}"
    );

    let closed = ask(
        &mut c,
        ControlRequest::CloseTerminal {
            id: "ct1".into(),
            terminal: first,
        },
    )
    .await;
    assert!(
        matches!(closed, ControlResponse::TerminalClosed { .. }),
        "{closed:?}"
    );
    assert!(d.terminals().get(first).is_none(), "the close woke the tab");
    let row = spaces(&mut c)
        .await
        .into_iter()
        .find(|s| s.id == space)
        .expect("the space");
    assert!(row.asleep, "closing a sleeping tab woke its space");
    assert_eq!(
        row.sleeping.iter().map(|t| t.terminal).collect::<Vec<_>>(),
        vec![second],
        "the closed tab is still listed, or the other one is gone"
    );

    let woke = wake(&mut c, space).await;
    assert!(
        matches!(woke, ControlResponse::WorkspaceAwake { terminals: 1, .. }),
        "{woke:?}"
    );
    assert!(d.terminals().get(first).is_none(), "a closed tab came back");
    assert!(d.terminals().get(second).is_some());
}

#[tokio::test]
async fn a_tab_made_in_a_sleeping_space_leaves_the_others_asleep() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("maker").await;
    let space = new_space(&mut c, "made").await;
    let old = new_tab(&mut c, space, "t1").await;
    let asleep = sleep(&mut c, space).await;
    assert!(
        matches!(asleep, ControlResponse::WorkspaceAsleep { .. }),
        "{asleep:?}"
    );
    let fresh = new_tab(&mut c, space, "t2").await;
    assert!(
        d.terminals().get(old).is_none(),
        "a tab made beside a sleeping one woke it"
    );
    assert!(d.terminals().get(fresh).is_some());
    let row = spaces(&mut c)
        .await
        .into_iter()
        .find(|s| s.id == space)
        .expect("the space");
    assert!(
        !row.asleep,
        "a space with an awake tab reads as asleep: {row:?}"
    );
    assert_eq!(
        row.sleeping.iter().map(|t| t.terminal).collect::<Vec<_>>(),
        vec![old],
        "{row:?}"
    );
}

/// The terminals an answer says changed.
fn changed(answer: &ControlResponse) -> Vec<u32> {
    match answer {
        ControlResponse::WorkspaceAsleep { changed, .. }
        | ControlResponse::WorkspaceAwake { changed, .. } => {
            changed.clone().expect("this daemon says what changed")
        }
        other => panic!("not a sleep or a wake: {other:?}"),
    }
}

fn split(first: u32, second: u32) -> Pane {
    Pane::Split {
        dir: SplitDir::V,
        permille: 500,
        first: Box::new(Pane::Leaf { terminal: first }),
        second: Box::new(Pane::Leaf { terminal: second }),
    }
}

#[tokio::test]
async fn one_tab_sleeps_and_wakes_alone_beside_an_awake_one() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("alone").await;
    let space = new_space(&mut c, "mixed").await;
    let resting = new_tab(&mut c, space, "t1").await;
    let busy = new_tab(&mut c, space, "t2").await;
    let slept = sleep_tab(&mut c, space, Some(resting)).await;
    assert_eq!(changed(&slept), vec![resting], "{slept:?}");
    assert!(d.terminals().get(resting).is_none());
    assert!(
        d.terminals().get(busy).is_some(),
        "the other tab went down too"
    );
    let row = spaces(&mut c)
        .await
        .into_iter()
        .find(|s| s.id == space)
        .expect("the space");
    assert!(!row.asleep, "{row:?}");
    assert_eq!(row.terminals, 1, "{row:?}");
    assert_eq!(
        row.sleeping
            .iter()
            .map(|t| (t.terminal, t.lead))
            .collect::<Vec<_>>(),
        vec![(resting, Some(resting))]
    );
    // Asking again is not an error and changes nothing.
    let again = sleep_tab(&mut c, space, Some(resting)).await;
    assert_eq!(changed(&again), Vec::<u32>::new(), "{again:?}");
    let woke = wake_tab(&mut c, space, Some(resting)).await;
    assert_eq!(changed(&woke), vec![resting], "{woke:?}");
    assert!(
        d.terminals().get(resting).is_some(),
        "the tab did not come back"
    );
    let woke = wake_tab(&mut c, space, Some(resting)).await;
    assert_eq!(
        changed(&woke),
        Vec::<u32>::new(),
        "a second wake opened it twice"
    );
}

#[tokio::test]
async fn a_split_tab_sleeps_and_wakes_whole_and_takes_no_new_pane_asleep() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("split").await;
    let space = new_space(&mut c, "panes").await;
    let first = new_tab(&mut c, space, "t1").await;
    let second = new_tab(&mut c, space, "t2").await;
    let other = new_tab(&mut c, space, "t3").await;
    let saved = ask(
        &mut c,
        ControlRequest::SetSplit {
            id: "split".into(),
            tree: split(first, second),
        },
    )
    .await;
    assert!(
        matches!(saved, ControlResponse::SplitSaved { .. }),
        "{saved:?}"
    );
    // Either pane names the tab.
    let slept = sleep_tab(&mut c, space, Some(second)).await;
    let mut ended = changed(&slept);
    ended.sort_unstable();
    assert_eq!(ended, vec![first, second], "{slept:?}");
    assert!(d.terminals().get(other).is_some());
    let row = spaces(&mut c)
        .await
        .into_iter()
        .find(|s| s.id == space)
        .expect("the space");
    assert!(
        row.sleeping.iter().all(|t| t.lead == Some(first)),
        "the panes of one tab do not share it: {row:?}"
    );
    // A live tab cannot be split onto a sleeping one.
    let joined = ask(
        &mut c,
        ControlRequest::SetSplit {
            id: "join".into(),
            tree: split(other, first),
        },
    )
    .await;
    assert!(
        matches!(joined, ControlResponse::Failed { .. }),
        "{joined:?}"
    );
    let woke = wake_tab(&mut c, space, Some(first)).await;
    let mut back = changed(&woke);
    back.sort_unstable();
    assert_eq!(back, vec![first, second], "{woke:?}");
    let listed = ask(
        &mut c,
        ControlRequest::ListAgents {
            id: "agents".into(),
        },
    )
    .await;
    let ControlResponse::Agents { splits, .. } = listed else {
        panic!("no index: {listed:?}");
    };
    assert!(
        splits
            .iter()
            .any(|tree| tree.leaves() == vec![first, second]),
        "the split did not come back with its panes: {splits:?}"
    );
}

#[tokio::test]
async fn a_space_sleeps_what_it_can_and_says_what_stayed_awake() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("best").await;
    let space = new_space(&mut c, "partly").await;
    let idle = new_tab(&mut c, space, "t1").await;
    let busy = new_tab(&mut c, space, "t2").await;
    c.input_to(busy, b"sleep 100\n").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let answer = sleep(&mut c, space).await;
    let ControlResponse::WorkspaceAsleep {
        changed, stayed, ..
    } = &answer
    else {
        panic!("nothing slept: {answer:?}");
    };
    assert_eq!(
        changed.clone().unwrap_or_default(),
        vec![idle],
        "{answer:?}"
    );
    assert_eq!(
        stayed.iter().map(|s| s.terminal).collect::<Vec<_>>(),
        vec![busy],
        "{answer:?}"
    );
    assert!(d.terminals().get(busy).is_some());
}

#[tokio::test]
async fn a_wake_with_nothing_named_brings_back_the_tab_that_slept_last() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("last").await;
    let space = new_space(&mut c, "order").await;
    let early = new_tab(&mut c, space, "t1").await;
    let late = new_tab(&mut c, space, "t2").await;
    sleep_tab(&mut c, space, Some(early)).await;
    // A second apart, so the order is the clock's and not the numbers'.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    sleep_tab(&mut c, space, Some(late)).await;
    let woke = ask(
        &mut c,
        ControlRequest::WakeWorkspace {
            id: "last".into(),
            workspace: None,
            terminal: None,
        },
    )
    .await;
    assert_eq!(changed(&woke), vec![late], "{woke:?}");
    assert!(
        d.terminals().get(early).is_none(),
        "the earlier tab woke too"
    );
}

#[tokio::test]
async fn a_sleeping_tab_stays_asleep_across_a_restart_beside_an_awake_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        dir.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    qoop_runtime::paths::ensure_private_dir(dir.path()).expect("private dir");

    async fn serve(
        dir: &std::path::Path,
    ) -> (
        TestClient,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<qoop_runtime::daemon::TerminalRegistry>,
    ) {
        let daemon = {
            let _binding = qoop_testkit::BINDING.lock().await;
            qoop_runtime::Daemon::bind(dir, std::path::Path::new("/bin/sh"))
                .await
                .expect("bind")
        };
        let terminals = daemon.terminals();
        let socket = daemon.socket().to_path_buf();
        let serving = tokio::spawn(async move {
            let _ = daemon.serve().await;
        });
        let (stream, inbuf) = qoop_runtime::client::connect(&socket, "restart")
            .await
            .expect("connect");
        (TestClient::from_parts(stream, inbuf, 1), serving, terminals)
    }

    let (space, tab) = {
        let (mut c, serving, _terminals) = serve(dir.path()).await;
        let space = new_space(&mut c, "overnight").await;
        let tab = new_tab(&mut c, space, "t1").await;
        let pane = new_tab(&mut c, space, "t2").await;
        // A third tab stays awake: the space is half asleep across the restart.
        new_tab(&mut c, space, "t3").await;
        let saved = ask(
            &mut c,
            ControlRequest::SetSplit {
                id: "split".into(),
                tree: Pane::Split {
                    dir: SplitDir::V,
                    permille: 500,
                    first: Box::new(Pane::Leaf { terminal: tab }),
                    second: Box::new(Pane::Leaf { terminal: pane }),
                },
            },
        )
        .await;
        assert!(
            matches!(saved, ControlResponse::SplitSaved { .. }),
            "{saved:?}"
        );
        let asleep = sleep_tab(&mut c, space, Some(tab)).await;
        assert!(
            matches!(
                asleep,
                ControlResponse::WorkspaceAsleep { terminals: 2, .. }
            ),
            "{asleep:?}"
        );
        drop(c);
        serving.abort();
        // Gone for certain, listener and all, before the next daemon binds the socket: the
        // task is over when it has been awaited, and the listener a moment after.
        let _ = serving.await;
        qoop_testkit::wait_until_unlistened(&qoop_runtime::socket_path(dir.path())).await;
        (space, tab)
    };

    let (mut c, serving, terminals) = serve(dir.path()).await;
    assert!(
        terminals.get(tab).is_none(),
        "a sleeping tab was woken by a restart"
    );
    assert!(
        terminals.get(tab + 2).is_some(),
        "the awake tab beside it did not come back"
    );
    assert!(
        dir.path().join("history").join(tab.to_string()).exists(),
        "the restart swept a sleeping tab's history"
    );
    let row = spaces(&mut c)
        .await
        .into_iter()
        .find(|s| s.id == space)
        .expect("the space");
    assert!(!row.asleep, "{row:?}");
    assert_eq!(row.sleeping.len(), 2, "{row:?}");
    let woke = wake(&mut c, space).await;
    assert!(
        matches!(woke, ControlResponse::WorkspaceAwake { terminals: 2, .. }),
        "{woke:?}"
    );
    assert!(terminals.get(tab).is_some(), "the tab did not come back");
    // The split slept through the restart with its tabs, and is there when they are.
    let listed = ask(
        &mut c,
        ControlRequest::ListAgents {
            id: "agents".into(),
        },
    )
    .await;
    let ControlResponse::Agents { splits, .. } = listed else {
        panic!("no index: {listed:?}");
    };
    assert!(
        splits
            .iter()
            .any(|tree| tree.leaves() == vec![tab, tab + 1]),
        "the split did not survive the sleep and the restart: {splits:?}"
    );
    serving.abort();
}

#[tokio::test]
async fn two_wakes_at_once_bring_each_tab_back_once() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("one").await;
    let mut other = d.attach("two").await;
    let space = new_space(&mut c, "twice").await;
    let tab = new_tab(&mut c, space, "t1").await;
    let asleep = sleep(&mut c, space).await;
    assert!(
        matches!(asleep, ControlResponse::WorkspaceAsleep { .. }),
        "{asleep:?}"
    );
    let w1 = ControlRequest::WakeWorkspace {
        id: "w1".into(),
        workspace: Some(space),
        terminal: None,
    };
    let w2 = ControlRequest::WakeWorkspace {
        id: "w2".into(),
        workspace: Some(space),
        terminal: None,
    };
    let (first, second) = tokio::join!(c.control(&w1, PATIENCE), other.control(&w2, PATIENCE));
    let opened: usize = [first, second]
        .into_iter()
        .map(|answer| match answer {
            Some(ControlResponse::WorkspaceAwake { terminals, .. }) => terminals,
            other => panic!("a wake failed: {other:?}"),
        })
        .sum();
    assert_eq!(opened, 1, "the tab was opened by both wakes");
    assert!(d.terminals().get(tab).is_some());
}

#[tokio::test]
async fn a_retried_close_of_a_sleeping_tab_answers_as_the_first_did() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("retry").await;
    let space = new_space(&mut c, "retried").await;
    let first = new_tab(&mut c, space, "t1").await;
    let _second = new_tab(&mut c, space, "t2").await;
    let asleep = sleep(&mut c, space).await;
    assert!(
        matches!(asleep, ControlResponse::WorkspaceAsleep { .. }),
        "{asleep:?}"
    );
    for attempt in 0..2 {
        let closed = ask(
            &mut c,
            ControlRequest::CloseTerminal {
                id: "same-close".into(),
                terminal: first,
            },
        )
        .await;
        assert!(
            matches!(closed, ControlResponse::TerminalClosed { .. }),
            "attempt {attempt}: {closed:?}"
        );
    }
}

async fn named_tab(c: &mut TestClient, workspace: i64, name: &str) -> u32 {
    match ask(
        c,
        ControlRequest::CreateTerminal {
            id: format!("tab-{workspace}-{name}"),
            cwd: None,
            workspace: Some(workspace),
            name: Some(name.into()),
        },
    )
    .await
    {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        other => panic!("expected a created terminal, got {other:?}"),
    }
}

/// Only qoop's own board page sleeps as a shell would: a program that calls itself `qoop`
/// and is handed the page's very words, but is not this daemon's binary, keeps its tab awake.
#[tokio::test]
async fn a_program_posing_as_the_board_page_keeps_its_tab_awake() {
    let scratch = tempfile::tempdir().expect("tempdir");
    // A real binary that blocks on its words: `yes`, under the name `qoop`.
    let posing = scratch.path().join("qoop");
    std::os::unix::fs::symlink("/usr/bin/yes", &posing).expect("yes as qoop");
    let d = TestDaemon::start().await;
    let mut c = d.attach("posing").await;
    let space = new_space(&mut c, "qass").await;
    let tab = named_tab(&mut c, space, "board").await;
    c.input_to(
        tab,
        format!(
            "exec '{}' task board --page > /dev/null\n",
            posing.display()
        )
        .as_bytes(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let answer = sleep(&mut c, space).await;
    assert!(
        matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains("is running in it")),
        "a program posing as the board page was put to sleep: {answer:?}"
    );
    assert!(d.terminals().get(tab).is_some(), "the refused tab ended");
}

/// A wake types the board page into the one row it belongs in: qass's space, the tab named
/// `board`, with no agent. A tab of that name in another space, and another tab beside it in
/// qass's space, come back as plain shells.
#[tokio::test]
async fn a_wake_types_the_board_page_only_into_qasss_board_tab() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let exe = scratch.path().join("qoop-page");
    std::fs::write(
        &exe,
        "#!/bin/sh\necho \"PAGE-UP-$1-$2\"\nwhile read -r l; do :; done\n",
    )
    .expect("the stand-in");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let d = TestDaemon::start_moving_onto(Path::new("/bin/sh"), Some(&exe)).await;
    let mut c = d.attach("page").await;
    let qass = new_space(&mut c, "qass").await;
    let dev = new_space(&mut c, "dev").await;
    let board = named_tab(&mut c, qass, "board").await;
    let notes = named_tab(&mut c, qass, "notes").await;
    let elsewhere = named_tab(&mut c, dev, "board").await;
    for space in [qass, dev] {
        let slept = sleep(&mut c, space).await;
        assert!(
            matches!(slept, ControlResponse::WorkspaceAsleep { .. }),
            "{slept:?}"
        );
        let woke = wake(&mut c, space).await;
        assert!(!changed(&woke).is_empty(), "{woke:?}");
    }
    let subscribed = ask(
        &mut c,
        ControlRequest::Subscribe {
            id: "again".into(),
            terminals: vec![board, notes, elsewhere],
        },
    )
    .await;
    assert!(
        matches!(subscribed, ControlResponse::Subscribed { .. }),
        "{subscribed:?}"
    );
    assert!(
        c.wait_for_text_on(board, "PAGE-UP-task-board", PATIENCE)
            .await,
        "the board page never came back in qass's board tab"
    );
    for tab in [notes, elsewhere] {
        c.input_to(tab, format!("echo PLAIN-{tab}-$((1+1))\n").as_bytes())
            .await;
        assert!(
            c.wait_for_text_on(tab, &format!("PLAIN-{tab}-2"), PATIENCE)
                .await
        );
        let screen: String = c
            .screen(tab)
            .map(|s| s.lines.join("\n"))
            .unwrap_or_default();
        assert!(
            !screen.contains("board --page") && !screen.contains("PAGE-UP"),
            "tab {tab} was typed the page: {screen}"
        );
    }
}

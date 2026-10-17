//! `ShowOpen`: a path or url the owner tapped in a chat, recorded for the phone apart from
//! the agents' shows.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage, ShowInfo};
use qoop_testkit::{TestClient, TestDaemon};

const PATIENCE: Duration = Duration::from_secs(10);

/// A home (`h`) and a `$TMPDIR` (`t`) of this test binary's own, so `~` and the roots of a
/// load nobody tapped are known places, and nothing under the real home is touched. A static
/// is never dropped, so its two empty folders stay in the system's temp folder.
fn sandbox() -> &'static Path {
    static SANDBOX: OnceLock<tempfile::TempDir> = OnceLock::new();
    SANDBOX
        .get_or_init(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            for sub in ["h", "t"] {
                std::fs::create_dir(dir.path().join(sub)).expect("mkdir");
            }
            // SAFETY: every test here calls this first, before its daemon exists, and the
            // others wait on the lock meanwhile; nothing writes the environment after it.
            unsafe {
                std::env::set_var("HOME", dir.path().join("h"));
                std::env::set_var("TMPDIR", dir.path().join("t"));
            }
            dir
        })
        .path()
}

fn home() -> std::path::PathBuf {
    sandbox().join("h")
}

/// A folder under the sandbox's home: outside `/tmp` and `$TMPDIR`.
fn in_home() -> tempfile::TempDir {
    tempfile::tempdir_in(home()).expect("tempdir")
}

/// A terminal started in `cwd`, once the daemon knows its folder.
async fn terminal_in(d: &TestDaemon, phone: &mut TestClient, cwd: &Path) -> u32 {
    let created = phone
        .control(
            &ControlRequest::CreateTerminal {
                id: "tab".into(),
                cwd: Some(cwd.display().to_string()),
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await;
    let Some(ControlResponse::TerminalCreated { terminal, .. }) = created else {
        panic!("no terminal: {created:?}");
    };
    let by = std::time::Instant::now() + PATIENCE;
    while d.terminals().cwd_of(terminal).is_none() {
        assert!(std::time::Instant::now() < by, "the folder was never read");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    terminal
}

/// The answer to a `ShowOpen`. The call is boxed, as in `fetch`: a test that opens many keeps
/// its own future small enough for a test thread's stack in a debug build.
async fn open(
    phone: &mut TestClient,
    terminal: u32,
    target: &str,
    auto: bool,
) -> Result<ShowInfo, String> {
    let request = ControlRequest::ShowOpen {
        id: "open".into(),
        terminal,
        target: target.into(),
        auto,
    };
    match Box::pin(phone.control(&request, PATIENCE)).await {
        Some(ControlResponse::Shows { mut shows, .. }) if shows.len() == 1 => Ok(shows.remove(0)),
        Some(ControlResponse::Failed { reason, .. }) => Err(reason),
        other => panic!("ShowOpen {target}: {other:?}"),
    }
}

async fn fetch(phone: &mut TestClient, show: u64) -> Vec<u8> {
    let request = ControlRequest::ShowFetch {
        id: "fetch".into(),
        show,
        offset: 0,
        len: 1 << 20,
    };
    match Box::pin(phone.control(&request, PATIENCE)).await {
        Some(ControlResponse::ShowChunk { data, .. }) => {
            qoop_runtime::microphone::decode(&data).expect("base64")
        }
        other => panic!("ShowFetch {show}: {other:?}"),
    }
}

async fn listed(phone: &mut TestClient) -> Vec<u64> {
    match phone
        .control(&ControlRequest::ShowRead { id: "read".into() }, PATIENCE)
        .await
    {
        Some(ControlResponse::Shows { shows, .. }) => shows.iter().map(|s| s.show).collect(),
        other => panic!("ShowRead: {other:?}"),
    }
}

#[tokio::test]
async fn a_tapped_path_is_read_from_the_terminals_folder_or_home_and_is_nobodys_news() {
    sandbox();
    let work = in_home();
    std::fs::create_dir(work.path().join("sub")).expect("mkdir");
    std::fs::write(work.path().join("sub/a.txt"), "relative").expect("a");
    let away = in_home();
    std::fs::write(away.path().join("notes.md"), "from home").expect("notes");
    let d = TestDaemon::start().await;
    let mut phone = d.attach_phone().await;
    let terminal = terminal_in(&d, &mut phone, work.path()).await;

    let rel = open(&mut phone, terminal, "sub/a.txt", false)
        .await
        .unwrap();
    assert_eq!((rel.kind.as_str(), rel.name.as_str()), ("file", "a.txt"));
    assert_eq!(rel.terminal, terminal);
    assert_eq!(rel.space, None);
    assert_eq!(fetch(&mut phone, rel.show).await, b"relative");
    let name = away.path().file_name().unwrap().to_string_lossy();
    let tilde = open(&mut phone, terminal, &format!("~/{name}/notes.md"), false)
        .await
        .unwrap();
    assert_eq!(fetch(&mut phone, tilde.show).await, b"from home");

    // Neither is listed nor told: the phone asked, so the phone knows.
    assert_eq!(listed(&mut phone).await, Vec::<u64>::new());
    let news = |e: &EventMessage| matches!(e, EventMessage::ShowsChanged);
    assert!(
        phone
            .wait_for_event(news, Duration::from_secs(1))
            .await
            .is_none()
    );
    // An agent's show is both, on the same connection.
    let token = d.agents_arc().token_of(terminal).expect("a token");
    let mut agent = d.attach("qoop-show").await;
    let shown = agent
        .control(
            &ControlRequest::ShowWrite {
                id: "show".into(),
                terminal,
                token: token.as_str().to_string(),
                target: work.path().join("sub/a.txt").display().to_string(),
                note: None,
            },
            PATIENCE,
        )
        .await;
    let Some(ControlResponse::ShowWritten { show, .. }) = shown else {
        panic!("not shown: {shown:?}");
    };
    assert_ne!(show, rel.show, "an agent's show is its own row");
    assert!(phone.wait_for_event(news, PATIENCE).await.is_some());
    assert_eq!(listed(&mut phone).await, [show]);

    for (target, why) in [
        ("sub/none.txt", "no file"),
        ("sub", "is not a regular file"),
    ] {
        let refused = open(&mut phone, terminal, target, false).await.unwrap_err();
        assert!(refused.contains(why), "{target}: {refused}");
    }
}

#[tokio::test]
async fn the_same_file_is_the_same_show_until_it_changes() {
    sandbox();
    let work = in_home();
    let file = work.path().join("shot.png");
    std::fs::write(&file, "one").expect("shot");
    let d = TestDaemon::start().await;
    let mut phone = d.attach_phone().await;
    let terminal = terminal_in(&d, &mut phone, work.path()).await;

    let first = open(&mut phone, terminal, "shot.png", false).await.unwrap();
    assert_eq!(first.kind, "image");
    let absolute = file.display().to_string();
    for target in ["shot.png", "./shot.png", absolute.as_str()] {
        let again = open(&mut phone, terminal, target, false).await.unwrap();
        assert_eq!(again.show, first.show, "{target}");
    }
    std::fs::write(&file, "two!").expect("bigger");
    let bigger = open(&mut phone, terminal, "shot.png", false).await.unwrap();
    assert_ne!(bigger.show, first.show, "a new size");
    std::fs::write(&file, "six!").expect("same size");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .and_then(|f| f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)))
        .expect("mtime");
    let touched = open(&mut phone, terminal, "shot.png", false).await.unwrap();
    assert_ne!(touched.show, bigger.show, "a new modification time");
    assert_eq!(fetch(&mut phone, touched.show).await, b"six!");
}

#[tokio::test]
async fn a_load_nobody_tapped_comes_only_from_where_agents_put_results() {
    sandbox();
    let work = in_home();
    let away = in_home();
    std::fs::write(work.path().join("here.png"), "here").expect("here");
    std::fs::write(work.path().join("notes.txt"), "text").expect("notes");
    std::fs::write(work.path().join("big.png"), vec![0u8; (2 << 20) + 1]).expect("big");
    std::fs::write(away.path().join("away.png"), "away").expect("away");
    let tmp = tempfile::Builder::new().tempdir_in("/tmp").expect("tmp");
    std::fs::write(tmp.path().join("tmp.png"), "tmp").expect("tmp file");
    let mine = tempfile::tempdir().expect("TMPDIR");
    assert!(mine.path().starts_with(sandbox().join("t")));
    std::fs::write(mine.path().join("mine.png"), "mine").expect("mine");
    let d = TestDaemon::start().await;
    let mut phone = d.attach_phone().await;
    let terminal = terminal_in(&d, &mut phone, work.path()).await;
    let at_home = terminal_in(&d, &mut phone, &home()).await;

    let away_png = away.path().join("away.png").display().to_string();
    let tmp_png = tmp.path().join("tmp.png").display().to_string();
    let mine_png = mine.path().join("mine.png").display().to_string();
    assert!(open(&mut phone, terminal, "here.png", true).await.is_ok());
    // Only a picture of at most 2 MiB loads without a tap, even from the terminal's folder.
    for name in ["notes.txt", "big.png"] {
        let refused = open(&mut phone, terminal, name, true).await.unwrap_err();
        assert!(refused.contains("only on a tap"), "{name}: {refused}");
        assert!(
            open(&mut phone, terminal, name, false).await.is_ok(),
            "{name}"
        );
    }
    let outside = open(&mut phone, terminal, &away_png, true)
        .await
        .unwrap_err();
    assert!(outside.contains("only on a tap"), "{outside}");
    // A tap opens it, and that row does not let a later load in.
    assert!(open(&mut phone, terminal, &away_png, false).await.is_ok());
    assert!(open(&mut phone, terminal, &away_png, true).await.is_err());
    // `/tmp` is `/private/tmp` on macOS: compared canonical.
    let shown = open(&mut phone, terminal, &tmp_png, true).await.unwrap();
    assert_eq!(fetch(&mut phone, shown.show).await, b"tmp");
    assert!(open(&mut phone, terminal, &mine_png, true).await.is_ok());
    // A terminal in the home folder makes nothing under it a root.
    let name = away.path().file_name().unwrap().to_string_lossy();
    assert!(
        open(&mut phone, at_home, &format!("{name}/away.png"), true)
            .await
            .is_err()
    );
    assert!(open(&mut phone, at_home, &tmp_png, true).await.is_ok());
    let url = open(&mut phone, terminal, "https://github.com", true).await;
    assert!(url.unwrap_err().contains("only on a tap"));
    assert!(
        open(&mut phone, terminal, "https://github.com", false)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn only_what_the_phone_views_is_opened() {
    sandbox();
    let work = in_home();
    for name in [
        "page.html",
        "logo.svg",
        "data.xml",
        "tool.bin",
        "Makefile",
        "ok.rs",
    ] {
        std::fs::write(work.path().join(name), "x").expect("file");
    }
    // A link named like a picture is judged by what it points at.
    std::os::unix::fs::symlink(work.path().join("page.html"), work.path().join("fake.png"))
        .expect("link");
    let d = TestDaemon::start().await;
    let mut phone = d.attach_phone().await;
    let terminal = terminal_in(&d, &mut phone, work.path()).await;
    for name in [
        "page.html",
        "logo.svg",
        "data.xml",
        "tool.bin",
        "Makefile",
        "fake.png",
    ] {
        let refused = open(&mut phone, terminal, name, false).await.unwrap_err();
        assert!(refused.ends_with("opens on the Mac"), "{name}: {refused}");
    }
    assert_eq!(
        open(&mut phone, terminal, "ok.rs", false)
            .await
            .unwrap()
            .kind,
        "file"
    );
}

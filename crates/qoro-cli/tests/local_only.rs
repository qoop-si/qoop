//! Local only: qoro, never paired, does everything the interface does with an ssh remote, and
//! nothing of it reaches a hub.
//!
//! The machine has a saved hub (a `hub.json`, as after a pairing that never finished) that
//! points at a listener this test owns and counts: any connection to it fails the test. The
//! remote is reached through an `ssh` on PATH that runs the remote command in a second runtime
//! directory, as `remote_spaces.rs` does; everything after the transport is real. The real
//! interface also starts, in a terminal of its own, with that remote saved. Afterwards no
//! service file and no machine key may exist.
//!
//! The qoro app's release build runs this same file against its own binary (a `#[path]` module
//! in its tests), so the build every user installs is held to it, not only this repository's.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoro_remote::Remote;
use qoro_runtime::client::{self, Connection, Incoming};
use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_runtime::daemon::ClientWire;
use qoro_tui::mux::is_remote_workspace;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn qoro() -> &'static str {
    env!("CARGO_BIN_EXE_qoro")
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

/// A listener standing in for the hub, counting every connection it is offered.
fn counting_hub() -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming() {
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });
    (port, count)
}

struct Tui {
    rd: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    wr: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    connection: Connection,
    inbuf: BytesMut,
    unread: std::collections::VecDeque<Incoming>,
    control_seq: u32,
    seq: u32,
}

impl Tui {
    async fn ask(&mut self, request: ControlRequest) {
        let bytes = client::encode_control_seq(&request, &mut self.control_seq).expect("encode");
        self.wr.write_all(&bytes).await.expect("write");
    }

    async fn type_into(&mut self, terminal: u32, text: &str) {
        let wire = ClientWire::Input {
            bytes: text.as_bytes().to_vec(),
            from_terminal: None,
            from_token: None,
        };
        let bytes = client::encode_command_to(terminal, &wire, &mut self.seq).expect("encode");
        self.wr.write_all(&bytes).await.expect("write");
    }

    async fn until<T>(&mut self, what: &str, mut want: impl FnMut(&Incoming) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let fresh = self.connection.drain(&mut self.inbuf).expect("decode");
            self.unread.extend(fresh);
            while let Some(incoming) = self.unread.pop_front() {
                if let Some(found) = want(&incoming) {
                    return found;
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}");
            let mut read = [0u8; 64 * 1024];
            match tokio::time::timeout(left, self.rd.read(&mut read)).await {
                Ok(Ok(0)) => panic!("the mux closed while waiting for {what}"),
                Ok(Ok(n)) => self.inbuf.extend_from_slice(&read[..n]),
                Ok(Err(e)) => panic!("read: {e}"),
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    async fn shows(&mut self, terminal: u32, text: &str) {
        self.until(
            &format!("{text} on the remote screen"),
            |incoming| match incoming {
                Incoming::Render {
                    terminal: t,
                    snapshot,
                } if *t == terminal && snapshot.lines.iter().any(|l| l.contains(text)) => Some(()),
                _ => None,
            },
        )
        .await
    }

    /// Ask until the remote is up: the first ask can beat the ssh connection.
    async fn remote_space(&mut self) -> i64 {
        for round in 0..40 {
            let id = format!("space-{round}");
            self.ask(ControlRequest::CreateWorkspace {
                id: id.clone(),
                name: "devbox:work".into(),
            })
            .await;
            let made = self
                .until("the remote space", |incoming| match incoming {
                    Incoming::Control(ControlResponse::WorkspaceCreated { id: i, workspace })
                        if *i == id =>
                    {
                        Some(Some(*workspace))
                    }
                    Incoming::Control(ControlResponse::Failed { id: i, .. }) if *i == id => {
                        Some(None)
                    }
                    _ => None,
                })
                .await;
            if let Some(space) = made {
                return space;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("the remote never came up");
    }
}

#[test]
fn an_unpaired_qoro_works_locally_and_never_reaches_a_hub() {
    // A big future in a debug build: a thread with room for it, as remote_spaces.rs does.
    let ran = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(scenario)
        .expect("thread")
        .join();
    if let Err(panic) = ran {
        std::panic::resume_unwind(panic);
    }
}

fn scenario() {
    let home = private_dir();
    let local = private_dir();
    let remote = private_dir();
    let (hub_port, hub_hits) = counting_hub();

    // A saved hub, as if `qoro pair --hub` had once been asked and never finished.
    let config = home.path().join("config");
    std::fs::create_dir_all(config.join("qoro")).expect("config");
    std::fs::write(
        config.join("qoro/hub.json"),
        format!("{{\"url\": \"ws://127.0.0.1:{hub_port}\"}}"),
    )
    .expect("hub.json");

    // `ssh` runs the remote command against the remote's runtime directory, with the qoro under
    // test first on PATH.
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::os::unix::fs::symlink(qoro(), bin.join("qoro")).expect("link qoro");
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nfor a; do last=$a; done\nunset QORO_SESSION\nQORO_RUNTIME_DIR={} exec /bin/sh -c \"$last\"\n",
            remote.path().display()
        ),
    )
    .expect("ssh");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // SAFETY: set before the runtime or any other thread but the listener's exists; the
    // listener reads no environment.
    unsafe {
        // Nothing of a qoro or an agent this test may itself run inside reaches what it starts.
        for (key, _) in std::env::vars() {
            if key.starts_with("QORO_") || key.starts_with("CLAUDE") {
                std::env::remove_var(key);
            }
        }
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
        std::env::set_var("XDG_CACHE_HOME", home.path().join("cache"));
        std::env::set_var("QORO_REMOTES", home.path().join("remotes.json"));
        std::env::set_var("SHELL", "/bin/sh");
    }

    let mut daemon = Command::new(qoro())
        .arg("daemon")
        .env("QORO_RUNTIME_DIR", local.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("local daemon");
    let socket = local.path().join("sessions/default/qorod.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        if Instant::now() > deadline {
            let _ = daemon.kill();
            panic!("the local daemon never listened");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            let local_conn = client::connect(&socket, "local-only")
                .await
                .expect("connect");
            let (tui_side, mux_side) = tokio::io::duplex(1 << 20);
            tokio::spawn(qoro_tui::mux::serve(
                mux_side,
                Some(local_conn),
                vec![Remote {
                    name: "devbox".into(),
                    target: "devbox.example".into(),
                }],
                None,
            ));
            let (rd, wr) = tokio::io::split(tui_side);
            let mut tui = Tui {
                rd,
                wr,
                connection: Connection::new(),
                inbuf: BytesMut::new(),
                unread: Default::default(),
                control_seq: 0,
                seq: 0,
            };

            let space = tui.remote_space().await;
            assert!(is_remote_workspace(space), "made locally: {space}");
            tui.ask(ControlRequest::CreateTerminal {
                id: "tab".into(),
                cwd: None,
                workspace: Some(space),
                name: None,
            })
            .await;
            let tab = tui
                .until("the remote tab", |incoming| match incoming {
                    Incoming::Control(ControlResponse::TerminalCreated { id, terminal })
                        if id == "tab" =>
                    {
                        Some(*terminal)
                    }
                    _ => None,
                })
                .await;
            tui.ask(ControlRequest::Subscribe {
                id: "show".into(),
                terminals: vec![tab],
            })
            .await;
            tui.type_into(tab, "echo local-only-$((6*7))\r").await;
            tui.shows(tab, "local-only-42").await;

            // Asleep and awake again, over ssh, with no cloud anywhere.
            tui.ask(ControlRequest::SleepWorkspace {
                id: "sleep".into(),
                workspace: space,
                terminal: Some(tab),
            })
            .await;
            tui.until("the remote tab asleep", |incoming| match incoming {
                Incoming::Control(ControlResponse::WorkspaceAsleep { id, changed, .. })
                    if id == "sleep" =>
                {
                    assert_eq!(changed.as_deref(), Some(&[tab][..]), "slept {changed:?}");
                    Some(())
                }
                Incoming::Control(ControlResponse::Failed { id, reason }) if id == "sleep" => {
                    panic!("the remote tab did not sleep: {reason}")
                }
                _ => None,
            })
            .await;
            tui.ask(ControlRequest::WakeWorkspace {
                id: "wake".into(),
                workspace: Some(space),
                terminal: Some(tab),
            })
            .await;
            tui.until("the remote tab awake", |incoming| match incoming {
                Incoming::Control(ControlResponse::WorkspaceAwake { id, .. }) if id == "wake" => {
                    Some(())
                }
                Incoming::Control(ControlResponse::Failed { id, reason }) if id == "wake" => {
                    panic!("the remote tab did not wake: {reason}")
                }
                _ => None,
            })
            .await;
            tui.ask(ControlRequest::Subscribe {
                id: "show-again".into(),
                terminals: vec![tab],
            })
            .await;
            tui.type_into(tab, "echo awake-$((6*7))\r").await;
            tui.shows(tab, "awake-42").await;
        })
    }));

    // The real interface, in a terminal of its own, with the remote saved: it starts and links
    // the remote, whose daemon (stopped first) its relay brings back up, and stays a while.
    let saved = Command::new(qoro())
        .args(["remote", "add", "devbox", "devbox.example"])
        .output()
        .expect("remote add");
    let _ = Command::new(qoro())
        .arg("stop")
        .env("QORO_RUNTIME_DIR", remote.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let remote_socket = remote.path().join("sessions/default/qorod.sock");
    let pty: Vec<&str> = if cfg!(target_os = "macos") {
        vec!["script", "-q", "/dev/null", qoro()]
    } else {
        vec!["script", "-qec", qoro(), "/dev/null"]
    };
    let mut interface = Command::new(pty[0])
        .args(&pty[1..])
        .env("QORO_RUNTIME_DIR", local.path())
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("interface");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut linked = false;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&remote_socket).is_ok() {
            linked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(3));
    let _ = interface.kill();
    let _ = interface.wait();

    // What install and every update run, and a look at the phones: quiet, and nothing written.
    let reload = Command::new(qoro())
        .args(["bridge", "reload"])
        .env("QORO_RUNTIME_DIR", local.path())
        .output()
        .expect("bridge reload");
    let devices = Command::new(qoro())
        .args(["devices", "ls"])
        .env("QORO_RUNTIME_DIR", local.path())
        .output()
        .expect("devices ls");
    for runtime in [local.path(), remote.path()] {
        let _ = Command::new(qoro())
            .arg("stop")
            .env("QORO_RUNTIME_DIR", runtime)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(saved.status.success(), "remote add: {saved:?}");
    assert!(linked, "the interface never linked the saved remote");

    assert!(
        reload.status.success() && reload.stdout.is_empty(),
        "bridge reload: {reload:?}"
    );
    // Answered by a build with the phone part, refused by one without: neither writes anything.
    // A build with the phone part answers with its (empty) list; one without says so, and its
    // `qoro pair` must fail without a connection to the saved hub either.
    let answer = String::from_utf8_lossy(&devices.stdout).into_owned()
        + &String::from_utf8_lossy(&devices.stderr);
    // The app's build runs this file too (its package is qoro-app): there the phone part must be.
    let app_build = env!("CARGO_PKG_NAME") == "qoro-app";
    if app_build || answer.contains("no phones paired") {
        assert!(
            devices.status.success() && answer.contains("no phones paired"),
            "devices ls: {devices:?}"
        );
    } else {
        assert!(
            answer.contains("no phone support"),
            "devices ls: {devices:?}"
        );
        let pair = Command::new(qoro())
            .args(["pair", "--hub", &format!("ws://127.0.0.1:{hub_port}")])
            .stdin(Stdio::null())
            .output()
            .expect("pair");
        assert!(
            !pair.status.success(),
            "pair without the phone part: {pair:?}"
        );
    }
    assert_eq!(
        hub_hits.load(Ordering::SeqCst),
        0,
        "something reached the saved hub on an unpaired machine"
    );
    let left: Vec<&Path> = [
        Path::new("Library/LaunchAgents/dev.qoro.bridge.plist"),
        Path::new(".config/systemd/user/qoro-bridge.service"),
        Path::new("config/systemd/user/qoro-bridge.service"),
        Path::new("data/qoro/machine.key"),
    ]
    .into_iter()
    .filter(|p| home.path().join(p).exists())
    .collect();
    assert!(
        left.is_empty(),
        "an unpaired machine got cloud files: {left:?}"
    );
}

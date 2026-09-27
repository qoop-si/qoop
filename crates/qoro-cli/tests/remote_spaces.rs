//! Remote spaces: the local daemon and a remote one behind one interface connection.
//!
//! The remote is reached through `ssh` as the product reaches it, except that `ssh` here is
//! a script that runs the remote command through the local `sh` against a second runtime
//! directory. Everything after the transport is real: the marker, the relay, the remote
//! daemon, the handshake, and the mux in between. What the script leaves out, the network
//! and a remote login shell, is covered in `qoro-remote` and by hand against a real sshd.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoro_remote::Remote;
use qoro_runtime::client::{self, Connection, Incoming};
use qoro_runtime::control::{ControlRequest, ControlResponse, WorkspaceRow};
use qoro_runtime::daemon::ClientWire;
use qoro_tui::mux::{is_remote_terminal, is_remote_workspace};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn qoro() -> &'static str {
    env!("CARGO_BIN_EXE_qoro")
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

fn stop(runtime: &Path) {
    let _ = Command::new(qoro())
        .arg("stop")
        .env("QORO_RUNTIME_DIR", runtime)
        .env_remove("QORO_SESSION")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A fake `ssh` on a PATH of its own: runs the last argument through `sh` with the remote's
/// runtime directory, and puts the qoro under test where the remote command looks for it.
fn fake_ssh(bin: &Path, remote_runtime: &Path) -> PathBuf {
    std::fs::create_dir_all(bin).expect("bin");
    std::os::unix::fs::symlink(qoro(), bin.join("qoro")).expect("link qoro");
    let script = bin.join("ssh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a; do last=$a; done\nunset QORO_SESSION\nQORO_RUNTIME_DIR={} exec /bin/sh -c \"$last\"\n",
            remote_runtime.display()
        ),
    )
    .expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

struct Tui {
    rd: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    wr: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    connection: Connection,
    inbuf: BytesMut,
    control_seq: u32,
    seqs: std::collections::BTreeMap<u32, u32>,
    spaces: Vec<WorkspaceRow>,
}

impl Tui {
    async fn ask(&mut self, request: ControlRequest) {
        let bytes = client::encode_control_seq(&request, &mut self.control_seq).expect("encode");
        self.wr.write_all(&bytes).await.expect("write");
    }

    async fn wire(&mut self, terminal: u32, wire: ClientWire) {
        let seq = self.seqs.entry(terminal).or_default();
        let bytes = client::encode_command_to(terminal, &wire, seq).expect("encode");
        self.wr.write_all(&bytes).await.expect("write");
    }

    /// Read until `want` says yes to something, within a deadline.
    async fn until<T>(&mut self, what: &str, mut want: impl FnMut(&Incoming) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for incoming in self.connection.drain(&mut self.inbuf).expect("decode") {
                if let Incoming::Control(ControlResponse::Workspaces { workspaces, .. }) = &incoming
                {
                    self.spaces = workspaces.clone();
                }
                if let Some(found) = want(&incoming) {
                    return found;
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}; spaces {:?}",
                self.spaces
            );
            let mut read = [0u8; 64 * 1024];
            match tokio::time::timeout(left, self.rd.read(&mut read)).await {
                Ok(Ok(0)) => panic!("the mux closed while waiting for {what}"),
                Ok(Ok(n)) => self.inbuf.extend_from_slice(&read[..n]),
                Ok(Err(e)) => panic!("read: {e}"),
                Err(_) => panic!("timed out waiting for {what}; spaces {:?}", self.spaces),
            }
        }
    }

    /// A merged space list whose remote rows satisfy `test`.
    async fn spaces_where(
        &mut self,
        what: &str,
        test: impl Fn(&[WorkspaceRow]) -> bool,
    ) -> Vec<WorkspaceRow> {
        self.until(what, |incoming| match incoming {
            Incoming::Control(ControlResponse::Workspaces { workspaces, .. })
                if test(workspaces) =>
            {
                Some(workspaces.clone())
            }
            _ => None,
        })
        .await
    }
}

fn remote_rows(spaces: &[WorkspaceRow]) -> Vec<&WorkspaceRow> {
    spaces
        .iter()
        .filter(|w| is_remote_workspace(w.id))
        .collect()
}

#[test]
fn a_remote_space_is_listed_driven_and_comes_back_after_a_drop() {
    let local = private_dir();
    let remote = private_dir();
    let home = private_dir();
    let ssh = fake_ssh(&home.path().join("bin"), remote.path());
    // One process-wide change, in a test binary of its own: the mux spawns `ssh` by name, and
    // its control sockets go under the cache directory, which must not be the real one.
    // SAFETY: set before the runtime or any other thread exists.
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:/usr/bin:/bin", ssh.parent().expect("bin").display()),
        );
        std::env::set_var("XDG_CACHE_HOME", home.path().join("cache"));
        std::env::remove_var("QORO_SESSION");
    }

    let mut daemon = Command::new(qoro())
        .arg("daemon")
        .env("QORO_RUNTIME_DIR", local.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("local daemon");
    let socket = local
        .path()
        .join("sessions")
        .join("default")
        .join("qorod.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "the local daemon never listened");
        std::thread::sleep(Duration::from_millis(50));
    }

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            let local_conn = client::connect(&socket, "test")
                .await
                .expect("connect local");
            let (tui_side, mux_side) = tokio::io::duplex(1 << 20);
            tokio::spawn(qoro_tui::mux::serve(
                mux_side,
                Some(local_conn),
                vec![Remote {
                    name: "devbox".into(),
                    target: "devbox.example".into(),
                    session: "default".into(),
                }],
            ));
            let (rd, wr) = tokio::io::split(tui_side);
            let mut tui = Tui {
                rd,
                wr,
                connection: Connection::new(),
                inbuf: BytesMut::new(),
                control_seq: 0,
                seqs: Default::default(),
                spaces: Vec::new(),
            };

            // A space made with the remote's prefix is made over there.
            tui.ask(ControlRequest::CreateWorkspace {
                id: "space-1".into(),
                name: "devbox:work".into(),
            })
            .await;
            let made = tui
                .until("the remote space to be made", |incoming| match incoming {
                    Incoming::Control(ControlResponse::WorkspaceCreated { id, workspace })
                        if id == "space-1" =>
                    {
                        Some(*workspace)
                    }
                    Incoming::Control(ControlResponse::Failed { id, reason })
                        if id == "space-1" =>
                    {
                        // Still connecting: say so and try again below.
                        let _ = reason;
                        Some(-1)
                    }
                    _ => None,
                })
                .await;
            let space = if made > 0 {
                made
            } else {
                // The first ask can beat the connection. Wait for it, then ask again.
                tui.spaces_where("the remote to come up", |s| {
                    remote_rows(s).iter().any(|w| !w.name.contains('('))
                })
                .await;
                tui.ask(ControlRequest::CreateWorkspace {
                    id: "space-2".into(),
                    name: "devbox:work".into(),
                })
                .await;
                tui.until("the remote space to be made", |incoming| match incoming {
                    Incoming::Control(ControlResponse::WorkspaceCreated { id, workspace })
                        if id == "space-2" =>
                    {
                        Some(*workspace)
                    }
                    _ => None,
                })
                .await
            };
            assert!(is_remote_workspace(space), "made locally: {space}");

            tui.ask(ControlRequest::CreateTerminal {
                id: "new-1".into(),
                cwd: Some("/definitely/a/local/path".into()),
                workspace: Some(space),
                name: None,
            })
            .await;
            let terminal = tui
                .until("the remote terminal", |incoming| match incoming {
                    Incoming::Control(ControlResponse::TerminalCreated { id, terminal })
                        if id == "new-1" =>
                    {
                        Some(*terminal)
                    }
                    Incoming::Control(ControlResponse::Failed { id, reason }) if id == "new-1" => {
                        panic!("the remote refused the terminal: {reason}")
                    }
                    _ => None,
                })
                .await;
            assert!(
                is_remote_terminal(terminal),
                "not a remote number: {terminal}"
            );

            tui.ask(ControlRequest::ListWorkspaces {
                id: "spaces".into(),
            })
            .await;
            let spaces = tui
                .spaces_where("the remote space in the merged list", |s| {
                    s.iter().any(|w| w.id == space && w.name == "devbox:work")
                })
                .await;
            assert!(
                spaces.iter().position(|w| !is_remote_workspace(w.id))
                    < spaces.iter().position(|w| is_remote_workspace(w.id)),
                "local spaces are listed first: {spaces:?}"
            );

            // Typed into and read back through the mux.
            tui.ask(ControlRequest::Subscribe {
                id: "show-tiles".into(),
                terminals: vec![terminal],
            })
            .await;
            tui.wire(
                terminal,
                ClientWire::Input {
                    bytes: b"echo remote-$((6*7))\r".to_vec(),
                    from_terminal: None,
                    from_token: None,
                },
            )
            .await;
            tui.until(
                "the remote screen to show the answer",
                |incoming| match incoming {
                    Incoming::Render {
                        terminal: t,
                        snapshot,
                    } if *t == terminal
                        && snapshot.lines.iter().any(|l| l.contains("remote-42")) =>
                    {
                        Some(())
                    }
                    _ => None,
                },
            )
            .await;

            // The remote really holds it, and the local daemon does not.
            let screen = |runtime: &Path| {
                let out = Command::new(qoro())
                    .args(["read", &(terminal & 0x00ff_ffff).to_string()])
                    .env("QORO_RUNTIME_DIR", runtime)
                    .env_remove("QORO_SESSION")
                    .output()
                    .expect("read");
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            assert!(
                screen(remote.path()).contains("remote-42"),
                "not on the remote"
            );
            assert!(
                !screen(local.path()).contains("remote-42"),
                "leaked to the local daemon"
            );

            // The remote goes away: its space stays listed, offline, under the same number.
            stop(remote.path());
            tui.spaces_where("the remote space to be offline", |s| {
                s.iter()
                    .any(|w| w.id == space && w.name.ends_with("(offline)"))
            })
            .await;

            // And comes back on its own: the relay starts its daemon again.
            tui.spaces_where("the remote space to be back", |s| {
                s.iter().any(|w| w.id == space && w.name == "devbox:work")
            })
            .await;

            // The local daemon going away ends the interface, as it does without a mux: the
            // stream closes rather than going quiet.
            stop(local.path());
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut read = [0u8; 64 * 1024];
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match tokio::time::timeout(left, tui.rd.read(&mut read)).await {
                    Ok(Ok(0)) | Ok(Err(_)) => break,
                    Ok(Ok(_)) => continue,
                    Err(_) => panic!("the interface was not told the local daemon went away"),
                }
            }
        });
    }));
    drop(rt);
    stop(remote.path());
    stop(local.path());
    let _ = daemon.kill();
    let _ = daemon.wait();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

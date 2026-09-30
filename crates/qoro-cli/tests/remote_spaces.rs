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

fn stop_session(runtime: &Path, session: &str) {
    let _ = Command::new(qoro())
        .args(["--session", session, "stop"])
        .env("QORO_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env_remove("QORO_SESSION")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn stop(runtime: &Path) {
    let _ = Command::new(qoro())
        .arg("stop")
        .env("QORO_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env_remove("QORO_SESSION")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A fake `ssh` on a PATH of its own: runs the last argument through `sh` with the remote's
/// runtime directory, and puts the qoro under test where the remote command looks for it.
fn fake_ssh(bin: &Path, remote_runtime: &Path, bare: &Path) -> PathBuf {
    std::fs::create_dir_all(bin).expect("bin");
    std::os::unix::fs::symlink(qoro(), bin.join("qoro")).expect("link qoro");
    // `bare.example` is a machine with no qoro: a home of its own, a PATH without it, and
    // a `uname` that says Linux on x86_64, so what it gets is the release for that.
    let shim = bare.join("shim");
    std::fs::create_dir_all(&shim).expect("shim");
    let uname = shim.join("uname");
    std::fs::write(
        &uname,
        "#!/bin/sh\ncase \"$1\" in -s) echo Linux ;; -m) echo x86_64 ;; esac\n",
    )
    .expect("uname");
    std::fs::set_permissions(&uname, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let run = bare.join("run");
    std::fs::create_dir_all(&run).expect("run");
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let script = bin.join("ssh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a; do host=$last; last=$a; done\nunset QORO_SESSION\n\
             if [ \"$host\" = bare.example ]; then HOME={} PATH={}:/usr/bin:/bin QORO_RUNTIME_DIR={} exec /bin/sh -c \"$last\"; fi\n\
             QORO_RUNTIME_DIR={} exec /bin/sh -c \"$last\"\n",
            bare.display(),
            shim.display(),
            run.display(),
            remote_runtime.display()
        ),
    )
    .expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

/// A release of this version in the API's own shape, served from a directory: the Linux
/// x86_64 tarball holds the qoro under test, with its checksum, so an install onto
/// `bare.example` goes through the real download, check and unpack.
fn fake_release(dir: &Path) -> String {
    let tags = dir.join("tags");
    std::fs::create_dir_all(&tags).expect("tags");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("src");
    std::fs::copy(qoro(), src.join("qoro")).expect("copy qoro");
    let asset = "qoro-x86_64-unknown-linux-gnu.tar.gz";
    let tarball = dir.join(asset);
    let packed = Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("qoro")
        .current_dir(&src)
        .status()
        .expect("tar");
    assert!(packed.success());
    let sums = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&tarball)
        .output()
        .expect("shasum");
    std::fs::write(dir.join(format!("{asset}.sha256")), &sums.stdout).expect("sums");
    let release = serde_json::json!({
        "tag_name": format!("v{}", env!("CARGO_PKG_VERSION")),
        "assets": [
            {"name": asset, "url": format!("file://{}", tarball.display())},
            {"name": format!("{asset}.sha256"), "url": format!("file://{}/{asset}.sha256", dir.display())},
        ],
    });
    std::fs::write(
        tags.join(format!("v{}", env!("CARGO_PKG_VERSION"))),
        release.to_string(),
    )
    .expect("release");
    format!("file://{}", dir.display())
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
    // One scenario, one future, and in a debug build that future is bigger than a test
    // thread's default stack: it runs on a thread with room for it.
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
    let local = private_dir();
    let remote = private_dir();
    let home = private_dir();
    let bare = private_dir();
    let ssh = fake_ssh(&home.path().join("bin"), remote.path(), bare.path());
    let releases = fake_release(&home.path().join("releases"));
    // One process-wide change, in a test binary of its own: the mux spawns `ssh` by name, and
    // its control sockets go under the cache directory, which must not be the real one.
    // SAFETY: set before the runtime or any other thread exists.
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:/usr/bin:/bin", ssh.parent().expect("bin").display()),
        );
        std::env::set_var("XDG_CACHE_HOME", home.path().join("cache"));
        std::env::set_var("QORO_REMOTES", home.path().join("remotes.json"));
        std::env::set_var("QORO_RELEASE_BASE", &releases);
        std::env::remove_var("QORO_SESSION");
    }

    let mut daemon = Command::new(qoro())
        .arg("daemon")
        .env("QORO_RUNTIME_DIR", local.path())
        .env("XDG_CONFIG_HOME", local.path().join("config"))
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
                    .env("XDG_CONFIG_HOME", runtime.join("config"))
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

            // From the `+`, by its address: asked whether qoro is there, then kept, named
            // after its host, and its spaces arrive without the interface restarting.
            tui.ask(ControlRequest::ConnectRemote {
                id: "connect-1".into(),
                target: "admin@10.0.0.5".into(),
            })
            .await;
            let name = tui
                .until("the machine to be added", |incoming| match incoming {
                    Incoming::Control(ControlResponse::RemoteAdded { id, name })
                        if id == "connect-1" =>
                    {
                        Some(name.clone())
                    }
                    Incoming::Control(ControlResponse::Failed { id, reason })
                        if id == "connect-1" =>
                    {
                        panic!("the machine was not added: {reason}")
                    }
                    _ => None,
                })
                .await;
            assert_eq!(name, "10.0.0.5");
            let kept = qoro_remote::saved::load_from(&home.path().join("remotes.json"))
                .expect("remotes saved");
            assert!(
                kept.iter().any(|r| r.target == "admin@10.0.0.5"),
                "the machine was not kept: {kept:?}"
            );
            tui.spaces_where("the new machine's space", |s| {
                s.iter()
                    .any(|w| is_remote_workspace(w.id) && w.name.starts_with("10.0.0.5:"))
            })
            .await;
            // Asking twice is "take me there": it answers with the one already there, under
            // its name, and adds nothing.
            tui.ask(ControlRequest::ConnectRemote {
                id: "connect-2".into(),
                target: "admin@10.0.0.5".into(),
            })
            .await;
            let again = tui
                .until(
                    "the second connect to name the first",
                    |incoming| match incoming {
                        Incoming::Control(ControlResponse::RemoteAdded { id, name })
                            if id == "connect-2" =>
                        {
                            Some(name.clone())
                        }
                        Incoming::Control(ControlResponse::Failed { id, reason })
                            if id == "connect-2" =>
                        {
                            panic!("the second connect failed: {reason}")
                        }
                        _ => None,
                    },
                )
                .await;
            assert_eq!(again, "10.0.0.5");
            let kept = qoro_remote::saved::load_from(&home.path().join("remotes.json"))
                .expect("remotes saved");
            assert_eq!(kept.len(), 1, "added twice: {kept:?}");
            stop_session(remote.path(), "work");

            Box::pin(install_onto_a_bare_machine(&mut tui, bare.path())).await;

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
    // The bare machine's daemon too: the connect after the install starts one there, under
    // `run`, and nothing else ends it. Left alone it outlives the test and its deleted
    // directory, one orphan per run.
    stop(&bare.path().join("run"));
    let _ = daemon.kill();
    let _ = daemon.wait();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// A machine with no qoro: named as such, installed from the release of this version,
/// then answering like any other; a second install never replaces it. Its own function,
/// boxed by the caller, because one async block holding every step of this test is a
/// future larger than a test thread's stack.
async fn install_onto_a_bare_machine(tui: &mut Tui, bare: &Path) {
    // A machine with no qoro: said so, with its platform, rather than failed or added.
    tui.ask(ControlRequest::ConnectRemote {
        id: "probe-bare".into(),
        target: "bare.example".into(),
    })
    .await;
    let platform = tui
        .until("the missing qoro to be named", |incoming| match incoming {
            Incoming::Control(ControlResponse::RemoteMissing {
                id, platform, root, ..
            }) if id == "probe-bare" => {
                assert!(!root);
                Some(platform.clone())
            }
            Incoming::Control(ControlResponse::Failed { id, reason }) if id == "probe-bare" => {
                panic!("the probe failed instead: {reason}")
            }
            _ => None,
        })
        .await;
    assert_eq!(platform, "linux x86_64");

    // Installed from the release of this version for that platform: downloaded,
    // checked, unpacked here, copied, run there, and only then linked.
    tui.ask(ControlRequest::InstallRemote {
        id: "install-1".into(),
        target: "bare.example".into(),
    })
    .await;
    let version = tui
        .until("the install", |incoming| match incoming {
            Incoming::Control(ControlResponse::RemoteInstalled { id, version, .. })
                if id == "install-1" =>
            {
                Some(version.clone())
            }
            Incoming::Control(ControlResponse::Failed { id, reason }) if id == "install-1" => {
                panic!("the install failed: {reason}")
            }
            _ => None,
        })
        .await;
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    let installed = bare.join(".local/bin/qoro");
    assert_eq!(
        std::fs::read(&installed).expect("installed"),
        std::fs::read(qoro()).expect("qoro"),
        "what arrived is not the binary the release holds"
    );
    let left: Vec<String> = std::fs::read_dir(bare.join(".local/bin"))
        .expect("bin")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        vec!["qoro".to_string()],
        "a staged copy was left behind"
    );

    // Now it answers like any machine with qoro, and is added.
    tui.ask(ControlRequest::ConnectRemote {
        id: "probe-bare-2".into(),
        target: "bare.example".into(),
    })
    .await;
    tui.until("the connect after the install", |incoming| match incoming {
        Incoming::Control(ControlResponse::RemoteAdded { id, .. }) if id == "probe-bare-2" => {
            Some(())
        }
        Incoming::Control(ControlResponse::Failed { id, reason }) if id == "probe-bare-2" => {
            panic!("the probe after the install failed: {reason}")
        }
        _ => None,
    })
    .await;

    // And a second install never replaces the qoro that is there.
    tui.ask(ControlRequest::InstallRemote {
        id: "install-2".into(),
        target: "bare.example".into(),
    })
    .await;
    tui.until(
        "a second install to be refused",
        |incoming| match incoming {
            Incoming::Control(ControlResponse::Failed { id, reason }) if id == "install-2" => {
                assert!(reason.contains("already"), "{reason}");
                Some(())
            }
            Incoming::Control(ControlResponse::RemoteInstalled { id, .. }) if id == "install-2" => {
                panic!("a second install replaced the qoro there")
            }
            _ => None,
        },
    )
    .await;
}

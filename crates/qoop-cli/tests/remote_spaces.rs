//! Remote spaces: the local daemon and a remote one behind one interface connection.
//!
//! The remote is reached through `ssh` as the product reaches it, except that `ssh` here is
//! a script that runs the remote command through the local `sh` against a second runtime
//! directory. Everything after the transport is real: the marker, the relay, the remote
//! daemon, the handshake, and the mux in between. What the script leaves out, the network
//! and a remote login shell, is covered in `qoop-remote` and by hand against a real sshd.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoop_remote::Remote;
use qoop_runtime::client::{self, Connection, Incoming};
use qoop_runtime::control::{ControlRequest, ControlResponse, WorkspaceRow};
use qoop_runtime::daemon::ClientWire;
use qoop_tui::mux::{is_remote_terminal, is_remote_workspace};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn qoop() -> &'static str {
    env!("CARGO_BIN_EXE_qoop")
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

fn stop_session(runtime: &Path, session: &str) {
    let _ = qoop_testkit::sandboxed(qoop())
        .args(["--session", session, "stop"])
        .env("QOOP_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env_remove("QOOP_SESSION")
        .env_remove("QORO_SESSION")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn stop(runtime: &Path) {
    let _ = qoop_testkit::sandboxed(qoop())
        .arg("stop")
        .env("QOOP_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env_remove("QOOP_SESSION")
        .env_remove("QORO_SESSION")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A fake `ssh` on a PATH of its own: runs the last argument through `sh` with the remote's
/// runtime directory, and puts the qoop under test where the remote command looks for it.
///
/// `old.example` is a machine whose qoop is older than this one, in a home of its own like
/// `bare.example`; every swap of a qoop run on any host is written to `swaps` by host.
fn fake_ssh(bin: &Path, remote_runtime: &Path, bare: &Path, old: &Path, swaps: &Path) -> PathBuf {
    std::fs::create_dir_all(bin).expect("bin");
    std::os::unix::fs::symlink(qoop(), bin.join("qoop")).expect("link qoop");
    // `bare.example` is a machine with no qoop: a home of its own, a PATH without it, and
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
            "#!/bin/sh\nfor a; do host=$last; last=$a; done\nunset QOOP_SESSION\nQOOP_TEST_SANDBOX=1; HOME=\"$(dirname \"$0\")/home\"; export QOOP_TEST_SANDBOX HOME; mkdir -p \"$HOME\"\n\
             case \"$last\" in *'qoop changed meanwhile'*) echo \"$host\" >> {};; esac\n\
             if [ \"$host\" = bare.example ]; then HOME={} PATH={}:/usr/bin:/bin QOOP_RUNTIME_DIR={} exec /bin/sh -c \"$last\"; fi\n\
             if [ \"$host\" = old.example ]; then HOME={} PATH={}:/usr/bin:/bin QOOP_RUNTIME_DIR={} exec /bin/sh -c \"$last\"; fi\n\
             QOOP_RUNTIME_DIR={} exec /bin/sh -c \"$last\"\n",
            swaps.display(),
            bare.display(),
            shim.display(),
            run.display(),
            old.display(),
            shim.display(),
            old.join("run").display(),
            remote_runtime.display()
        ),
    )
    .expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

/// A release of this version in the API's own shape, served from a directory: the Linux
/// x86_64 tarball holds the qoop under test, with its checksum, so an install onto
/// `bare.example` goes through the real download, check and unpack.
fn fake_release(dir: &Path) -> String {
    let tags = dir.join("tags");
    std::fs::create_dir_all(&tags).expect("tags");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("src");
    std::fs::copy(qoop(), src.join("qoop")).expect("copy qoop");
    let asset = "qoop-x86_64-unknown-linux-gnu.tar.gz";
    let tarball = dir.join(asset);
    let packed = Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("qoop")
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
    /// Messages decoded and not looked at yet. Kept across waits: one read can carry the
    /// message a wait is for and the next one after it, and a list dropped there is a wait
    /// that never ends (an "offline" list and the "back" one 50 ms later, read together
    /// once the test thread came back from a blocking `qoop stop`).
    unread: std::collections::VecDeque<Incoming>,
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
            let fresh = self.connection.drain(&mut self.inbuf).expect("decode");
            self.unread.extend(fresh);
            while let Some(incoming) = self.unread.pop_front() {
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

    /// Read everything that arrives for `long`, each through `look`.
    async fn quiet_for(&mut self, long: Duration, mut look: impl FnMut(&Incoming)) {
        let deadline = Instant::now() + long;
        loop {
            let fresh = self.connection.drain(&mut self.inbuf).expect("decode");
            self.unread.extend(fresh);
            while let Some(incoming) = self.unread.pop_front() {
                if let Incoming::Control(ControlResponse::Workspaces { workspaces, .. }) = &incoming
                {
                    self.spaces = workspaces.clone();
                }
                look(&incoming);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let mut read = [0u8; 64 * 1024];
            match tokio::time::timeout(left, self.rd.read(&mut read)).await {
                Ok(Ok(0)) => panic!("the mux closed"),
                Ok(Ok(n)) => self.inbuf.extend_from_slice(&read[..n]),
                Ok(Err(e)) => panic!("read: {e}"),
                Err(_) => return,
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
    let old = private_dir();
    let swaps = home.path().join("swaps");
    let ssh = fake_ssh(
        &home.path().join("bin"),
        remote.path(),
        bare.path(),
        old.path(),
        &swaps,
    );
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
        std::env::set_var("QOOP_REMOTES", home.path().join("remotes.json"));
        std::env::set_var("QOOP_RELEASE_BASE", &releases);
        std::env::remove_var("QOOP_SESSION");
    }

    let mut daemon = qoop_testkit::sandboxed(qoop())
        .arg("daemon")
        .env("QOOP_RUNTIME_DIR", local.path())
        .env("XDG_CONFIG_HOME", local.path().join("config"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("local daemon");
    let socket = local
        .path()
        .join("sessions")
        .join("default")
        .join("qoopd.sock");
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
            tokio::spawn(qoop_tui::mux::serve(
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
                let out = qoop_testkit::sandboxed(qoop())
                    .args(["read", &(terminal & 0x00ff_ffff).to_string()])
                    .env("QOOP_RUNTIME_DIR", runtime)
                    .env("XDG_CONFIG_HOME", runtime.join("config"))
                    .env_remove("QOOP_SESSION")
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

            // From the `+`, by its address: asked whether qoop is there, then kept, named
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
            let kept = qoop_remote::saved::load_from(&home.path().join("remotes.json"))
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
            let kept = qoop_remote::saved::load_from(&home.path().join("remotes.json"))
                .expect("remotes saved");
            assert_eq!(kept.len(), 1, "added twice: {kept:?}");

            // Added from a shell (`qoop remote add`) while this interface runs: written to
            // the list by someone else, and its spaces arrive without a restart or a `+`.
            let file = home.path().join("remotes.json");
            let mut list = qoop_remote::saved::load_from(&file).expect("remotes saved");
            list.push(Remote {
                name: "shelled".into(),
                target: "shelled.example".into(),
            });
            qoop_remote::saved::save_to(&file, &list).expect("add from a shell");
            tui.spaces_where("a remote added from a shell", |s| {
                s.iter()
                    .any(|w| is_remote_workspace(w.id) && w.name.starts_with("shelled:"))
            })
            .await;
            stop_session(remote.path(), "work");

            Box::pin(install_onto_a_bare_machine(&mut tui, bare.path())).await;
            Box::pin(push_onto_an_older_machine(&mut tui, old.path(), &swaps)).await;

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
    stop(&old.path().join("run"));
    let _ = daemon.kill();
    let _ = daemon.wait();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// A machine with no qoop: named as such, installed from the release of this version,
/// then answering like any other; a second install never replaces it. Its own function,
/// boxed by the caller, because one async block holding every step of this test is a
/// future larger than a test thread's stack.
async fn install_onto_a_bare_machine(tui: &mut Tui, bare: &Path) {
    // A machine with no qoop: said so, with its platform, rather than failed or added.
    tui.ask(ControlRequest::ConnectRemote {
        id: "probe-bare".into(),
        target: "bare.example".into(),
    })
    .await;
    let platform = tui
        .until("the missing qoop to be named", |incoming| match incoming {
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
    let installed = bare.join(".local/bin/qoop");
    assert_eq!(
        std::fs::read(&installed).expect("installed"),
        std::fs::read(qoop()).expect("qoop"),
        "what arrived is not the binary the release holds"
    );
    // The binary and the `qoro` link beside it for what still says the old name.
    let mut left: Vec<String> = std::fs::read_dir(bare.join(".local/bin"))
        .expect("bin")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec!["qoop".to_string(), "qoro".to_string()],
        "a staged copy was left behind"
    );

    // Now it answers like any machine with qoop, and is added.
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

    // And a second install never replaces the qoop that is there.
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
                panic!("a second install replaced the qoop there")
            }
            _ => None,
        },
    )
    .await;
}

/// A machine whose qoop is older than this one, running a daemon from before
/// `RestartWhenSafe`: the release of this version is put on its disk once, in place of the
/// old file, and its daemon is not asked to move (it would ignore that a move nobody asked
/// for ends no tab and closes no window): that is said once. The link coming back after that
/// pushes nothing and asks nothing again, and the machine of the same version never had
/// anything pushed at all.
async fn push_onto_an_older_machine(tui: &mut Tui, old: &Path, swaps: &Path) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // The old qoop: a script that says it is 0.0.1 and relays with the qoop under test.
    let bin = old.join(".local/bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n\
         *--version*) echo 'qoop 0.0.1' ;;\n\
         *--hello*) echo '{{\"version\":\"0.0.1\",\"codec_generation\":{},\"platform\":\"linux\"}}' ;;\n\
         *) exec {} \"$@\" ;;\nesac\n",
        qoop_proto::CODEC_GENERATION,
        qoop()
    );
    std::fs::write(bin.join("qoop"), script).expect("old qoop");
    std::fs::set_permissions(bin.join("qoop"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let sessions = old.join("run/sessions/default");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&sessions)
        .expect("sessions");
    for dir in [old.join("run"), old.join("run/sessions")] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    let asked = Arc::new(AtomicUsize::new(0));
    let welcomed = Arc::new(AtomicUsize::new(0));
    tokio::spawn(pre_feature_daemon(
        sessions.join("qoopd.sock"),
        asked.clone(),
        welcomed.clone(),
    ));

    tui.ask(ControlRequest::ConnectRemote {
        id: "connect-old".into(),
        target: "old.example".into(),
    })
    .await;
    let said = tui
        .until(
            "the older machine to be updated",
            |incoming| match incoming {
                Incoming::Control(ControlResponse::Failed { id, reason })
                    if id == "connect-old" =>
                {
                    panic!("the older machine was not added: {reason}")
                }
                Incoming::Control(ControlResponse::RestartArmed {
                    id,
                    waiting_on: Some(line),
                }) if id == "remote-update" => Some(line.clone()),
                _ => None,
            },
        )
        .await;
    assert!(
        said.starts_with("old.example ") && said.contains("when you run `qoop update`"),
        "{said}"
    );
    assert_eq!(
        std::fs::read(bin.join("qoop")).expect("swapped"),
        std::fs::read(qoop()).expect("qoop"),
        "the release was not put in place of the old qoop"
    );
    let left: Vec<String> = std::fs::read_dir(&bin)
        .expect("bin")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["qoop".to_string()], "a staged copy was left");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "an old daemon was asked to move"
    );

    // Still connected, more traffic to that machine: its refusals are its own, and the ask
    // to move is neither repeated nor said again.
    let space = tui
        .spaces
        .iter()
        .find(|w| w.name.starts_with("old.example:"))
        .map(|w| w.id)
        .expect("the older machine's space is listed");
    tui.ask(ControlRequest::CreateTerminal {
        id: "new-old".into(),
        cwd: None,
        workspace: Some(space),
        name: None,
    })
    .await;
    let refused = tui
        .until(
            "the older machine to refuse a new tab",
            |incoming| match incoming {
                Incoming::Control(ControlResponse::Failed { id, reason }) if id == "new-old" => {
                    Some(reason.clone())
                }
                Incoming::Control(ControlResponse::Failed { id, reason })
                    if id == "remote-update" =>
                {
                    panic!("said again: {reason}")
                }
                Incoming::Control(ControlResponse::RestartArmed { id, .. })
                    if id == "remote-update" =>
                {
                    panic!("said again")
                }
                _ => None,
            },
        )
        .await;
    assert_eq!(
        refused,
        "old.example runs an older qoop that cannot do this yet"
    );
    assert_eq!(welcomed.load(Ordering::SeqCst), 1, "the link dropped");
    assert_eq!(asked.load(Ordering::SeqCst), 0, "asked to move");

    // The daemon goes away and the link comes back to one still on 0.0.1: nothing is
    // pushed or asked again.
    let old_tab = qoop_tui::mux::REMOTE_TERMINAL | ((space >> 40) as u32) << 24 | 1;
    tui.ask(ControlRequest::CloseTerminal {
        id: "close-old".into(),
        terminal: old_tab,
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while welcomed.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline, "the link never came back");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tui.quiet_for(Duration::from_secs(2), |incoming| {
        if let Incoming::Control(ControlResponse::Failed { id, reason }) = incoming {
            assert_ne!(id, "remote-update", "said again: {reason}");
        }
        if let Incoming::Control(ControlResponse::RestartArmed { id, .. }) = incoming {
            assert_ne!(id, "remote-update", "said again");
        }
    })
    .await;
    assert_eq!(asked.load(Ordering::SeqCst), 0, "asked to move");
    let swapped = std::fs::read_to_string(swaps).unwrap_or_default();
    assert_eq!(
        swapped.lines().collect::<Vec<_>>(),
        vec!["old.example"],
        "pushed other than once to the older machine alone"
    );
}

/// A daemon from before `RestartWhenSafe`, at `socket`: it welcomes as `0.0.1`, lists one
/// space with a tab in it (so its link is never let go), and refuses the ask to move the way
/// an old daemon refuses any ask it does not know: under the ask's own id, staying
/// connected. A `CloseTerminal` is refused the same way and then the connection ends, as a
/// daemon going away would, so the link can be seen to come back without asking again.
async fn pre_feature_daemon(
    socket: PathBuf,
    asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    welcomed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use qoop_proto::{Channel, Frame, HandshakeOutcome, ServerCapabilities};
    use std::sync::atomic::Ordering;
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
    while let Ok((mut stream, _)) = listener.accept().await {
        let (asked, welcomed) = (asked.clone(), welcomed.clone());
        tokio::spawn(async move {
            let mut inbuf = BytesMut::new();
            let mut read = [0u8; 8192];
            let mut shaken = false;
            loop {
                match stream.read(&mut read).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => inbuf.extend_from_slice(&read[..n]),
                }
                while let Ok(Some(frame)) = Frame::decode(&mut inbuf) {
                    let mut hang_up = false;
                    let (channel, body) = if !shaken {
                        let Ok(hello) = serde_json::from_slice(&frame.body) else {
                            return;
                        };
                        let caps = ServerCapabilities {
                            version: "0.0.1".into(),
                            ..ServerCapabilities::first_generation()
                        };
                        let HandshakeOutcome::Welcome(welcome) =
                            qoop_proto::negotiate(&hello, &caps)
                        else {
                            return;
                        };
                        shaken = true;
                        welcomed.fetch_add(1, Ordering::SeqCst);
                        (Channel::Handshake, serde_json::to_vec(&welcome))
                    } else if frame.channel == Channel::Control {
                        let request: serde_json::Value =
                            serde_json::from_slice(&frame.body).unwrap_or_default();
                        let id = request["id"].as_str().unwrap_or_default().to_string();
                        let op = request["op"].as_str().unwrap_or_default();
                        let answer = match op {
                            "ListWorkspaces" => ControlResponse::Workspaces {
                                id,
                                workspaces: vec![WorkspaceRow {
                                    keep_awake: false,
                                    qass_skip: false,
                                    id: 1,
                                    name: "work".into(),
                                    terminals: 1,
                                    asleep: false,
                                    sleeping: Vec::new(),
                                    closable: true,
                                    ink: None,
                                    link: None,
                                    goal: None,
                                }],
                            },
                            "ListAgents" => ControlResponse::Agents {
                                id,
                                agents: Vec::new(),
                                splits: Vec::new(),
                            },
                            _ => {
                                if op == "RestartWhenSafe" {
                                    asked.fetch_add(1, Ordering::SeqCst);
                                }
                                hang_up = op == "CloseTerminal";
                                ControlResponse::Failed {
                                    id,
                                    reason: format!(
                                        "this daemon is older than the client that called it: \
                                         it does not know the method that was asked for \
                                         (unknown variant `{op}`)"
                                    ),
                                }
                            }
                        };
                        (Channel::Control, serde_json::to_vec(&answer))
                    } else {
                        continue;
                    };
                    let mut out = BytesMut::new();
                    Frame::new(channel, 0, bytes::Bytes::from(body.expect("json")))
                        .encode(&mut out)
                        .expect("encode");
                    if stream.write_all(&out).await.is_err() || hang_up {
                        return;
                    }
                }
            }
        });
    }
}

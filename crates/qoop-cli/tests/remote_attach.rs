//! Gates G6.3 and G6.6: attaching across a transport, and what a disconnect leaves behind.
//!
//! The transport here is `sh -c 'qoro relay'` rather than `ssh host 'qoro relay'`. That is
//! the same relay, the same protocol and the same pipe: what is missing is the network and
//! the remote's login shell, and both of those are covered in `qoop-remote` against real
//! login shells. What is covered here is what the network cannot change: whether a
//! disconnect mid-operation leaves the source workspace untouched, and whether three
//! machines appear in one index.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qoop_proto::{Channel, Frame};

fn qoro() -> &'static str {
    env!("CARGO_BIN_EXE_qoop")
}

struct Machine {
    dir: tempfile::TempDir,
    name: &'static str,
}

/// A machine's daemon outlives the relay that started it, which is the point of a daemon
/// and a leak in a test: `relay` starts one if there is not one, and killing the relay
/// leaves it holding a shell in a directory this test is about to delete. Measured after a
/// suite run: thirty two daemons still alive, each in a temporary directory that no longer
/// existed, so nothing could ever reach them again.
impl Drop for Machine {
    fn drop(&mut self) {
        let _ = Command::new(qoro())
            .arg("stop")
            .env("QORO_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Machine {
    fn new(name: &'static str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        Self { dir, name }
    }

    /// A relay, as ssh would start one on the far side.
    fn relay(&self) -> Child {
        Command::new("/bin/sh")
            .args(["-c", &format!("exec {} relay", qoro())])
            .env("QORO_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a relay")
    }

    /// Everything that would have to be identical after a failed operation.
    ///
    /// The state on disk, hashed whole. Not a count of files: a count is the same after a
    /// truncation, and the point of this hash is that anything at all having changed shows
    /// up.
    fn state_hash(&self) -> String {
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        collect(self.dir.path(), self.dir.path(), &mut entries);
        entries.sort();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (name, bytes) in entries {
            for byte in name.as_bytes().iter().chain(bytes.iter()) {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x1000_0000_01b3);
            }
        }
        format!("{hash:016x}")
    }
}

/// Each machine's state hash, once each has stopped changing on its own.
///
/// Two equal readings a beat apart, which is what "settled" can mean from the outside. The
/// deadline is a failure rather than a silent continue: a machine still writing after it is
/// up is a thing worth seeing, not a thing worth averaging over.
fn quiet(machines: &[Machine]) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut last: Vec<String> = machines.iter().map(Machine::state_hash).collect();
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
        let now: Vec<String> = machines.iter().map(Machine::state_hash).collect();
        if now == last {
            return now;
        }
        last = now;
    }
    panic!("a machine kept writing to its runtime directory after it was up");
}

fn collect(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        // The socket and the lock are runtime state, not workspace state: a socket's inode
        // changes on every daemon start and comparing it would make this hash useless.
        if name.ends_with(".sock") || name.ends_with(".lock") {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((name, bytes));
        }
    }
}

/// Stop a relay, and the daemon it started with it.
///
/// `qoro relay` starts a daemon if there is not one, as a child of itself, and does not
/// wait on it. Killing only the relay therefore leaves a daemon running against a runtime
/// directory that the test is about to delete: harmless once, and thirteen orphans holding
/// eighty megabytes after an afternoon of running the suite. The children are listed before
/// the relay dies, because afterwards they belong to init and nothing connects them to this
/// test any more.
fn stop(relay: &mut Child) {
    let listed = Command::new("pgrep")
        .arg("-P")
        .arg(relay.id().to_string())
        .output();
    let _ = relay.kill();
    let _ = relay.wait();
    let Ok(listed) = listed else { return };
    for pid in String::from_utf8_lossy(&listed.stdout).split_whitespace() {
        let _ = Command::new("kill").arg(pid).status();
    }
}

/// Speak the handshake over a relay and read the welcome back.
fn handshake(relay: &mut Child) -> Result<String, std::io::Error> {
    let hello = serde_json::json!({
        "codec_generation_min": qoop_proto::CODEC_GENERATION,
        "codec_generation_max": qoop_proto::CODEC_GENERATION,
        "client_id": "remote",
        "client_name": "test",
        "client_version": "0",
        "codecs": {
            "control": ["control.json.v1"],
            "event": ["event.json.v1"],
            "terminal": ["terminal.frame.v1"],
            "blob": ["blob.raw.v1"],
        },
        "extra": {},
    });
    let mut out = bytes::BytesMut::new();
    Frame::new(
        Channel::Handshake,
        0,
        bytes::Bytes::from(serde_json::to_vec(&hello).expect("encode")),
    )
    .encode(&mut out)
    .expect("encode");

    let stdin = relay.stdin.as_mut().expect("stdin");
    stdin.write_all(&out)?;
    stdin.flush()?;

    let stdout = relay.stdout.as_mut().expect("stdout");
    let mut buf = [0u8; 8192];
    let n = stdout.read(&mut buf)?;
    let mut inbuf = bytes::BytesMut::from(&buf[..n]);
    let frame = Frame::decode(&mut inbuf)
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .ok_or_else(|| std::io::Error::other("no frame in the welcome"))?;
    Ok(String::from_utf8_lossy(&frame.body).into_owned())
}

/// Ask a machine something over an already handshaken relay, and read the answer.
///
/// Not a second handshake: a connection handshakes once, and sending another one asks a
/// question nobody answers. The first version of this test did exactly that and read a
/// timeout as a machine that had stopped working.
fn control_ping(relay: &mut Child) -> Result<String, std::io::Error> {
    let request = serde_json::json!({ "op": "ListTerminals", "id": "ping" });
    let mut out = bytes::BytesMut::new();
    Frame::new(
        Channel::Control,
        0,
        bytes::Bytes::from(serde_json::to_vec(&request).expect("encode")),
    )
    .encode(&mut out)
    .expect("encode");

    let stdin = relay.stdin.as_mut().expect("stdin");
    stdin.write_all(&out)?;
    stdin.flush()?;

    // The render lane also writes to this pipe, so the answer is found by decoding rather
    // than by reading the next bytes.
    let stdout = relay.stdout.as_mut().expect("stdout");
    let mut inbuf = bytes::BytesMut::new();
    let mut buf = [0u8; 64 * 1024];
    for _ in 0..64 {
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            return Err(std::io::Error::other("the relay closed"));
        }
        inbuf.extend_from_slice(&buf[..n]);
        while let Some(frame) =
            Frame::decode(&mut inbuf).map_err(|e| std::io::Error::other(e.to_string()))?
        {
            if frame.channel == Channel::Control {
                return Ok(String::from_utf8_lossy(&frame.body).into_owned());
            }
        }
    }
    Err(std::io::Error::other("no control answer"))
}

/// G6.6: one local machine and two remotes appear in a single index, and a disconnect on
/// one leaves the other two untouched.
#[test]
fn g6_6_three_machines_in_one_index_and_a_disconnect_touches_one() {
    let machines = [
        Machine::new("local"),
        Machine::new("remote-a"),
        Machine::new("remote-b"),
    ];
    let mut relays: Vec<Child> = machines.iter().map(Machine::relay).collect();

    // Every machine answers, which is what makes them one index rather than three hopes.
    let mut index = Vec::new();
    for (machine, relay) in machines.iter().zip(relays.iter_mut()) {
        let welcome = handshake(relay).unwrap_or_else(|e| panic!("{}: {e}", machine.name));
        assert!(
            welcome.contains("codec"),
            "{} did not answer a handshake: {welcome}",
            machine.name
        );
        index.push(machine.name);
    }
    assert_eq!(index, vec!["local", "remote-a", "remote-b"]);

    // A baseline taken once each machine has stopped writing on its own.
    //
    // A daemon settles after it starts: it records where its first terminal is as soon as
    // that terminal's shell draws a prompt, which is a write that has nothing to do with
    // any other machine. Taken without waiting, the baseline lands in the middle of that
    // and the assertion below reads an ordinary startup write as a disconnect touching an
    // unrelated machine.
    let before = quiet(&machines);

    // One machine's connection dies abruptly, with no goodbye.
    stop(&mut relays[1]);
    std::thread::sleep(Duration::from_millis(800));

    // The other two still answer, asked over the connections they already have.
    for i in [0usize, 2] {
        let answer = control_ping(&mut relays[i]).unwrap_or_else(|e| {
            panic!(
                "{} stopped answering after a disconnect elsewhere: {e}",
                machines[i].name
            )
        });
        assert!(
            answer.contains("Terminals"),
            "{} answered {answer}",
            machines[i].name
        );
    }
    let after: Vec<String> = machines.iter().map(Machine::state_hash).collect();
    for i in [0usize, 2] {
        if before[i] != after[i] {
            let mut entries = Vec::new();
            collect(machines[i].dir.path(), machines[i].dir.path(), &mut entries);
            let names: Vec<(String, usize)> =
                entries.iter().map(|(n, b)| (n.clone(), b.len())).collect();
            panic!(
                "{}'s state changed when a different machine disconnected; it now holds \
                 {names:?}",
                machines[i].name
            );
        }
    }

    for mut relay in relays {
        stop(&mut relay);
    }
}

/// G6.3: a forced disconnect mid-operation leaves the source workspace byte identical.
///
/// Mid-operation is the part that matters: a disconnect between messages proves nothing,
/// because there is nothing in flight to leave half done.
#[test]
fn g6_3_a_forced_disconnect_leaves_the_source_byte_identical() {
    let machine = Machine::new("source");
    let mut relay = machine.relay();
    handshake(&mut relay).expect("handshake");
    std::thread::sleep(Duration::from_millis(500));
    let before = machine.state_hash();

    // A control request, cut off after the first byte. The daemon has a partial frame and
    // its peer is gone.
    let request = serde_json::json!({ "op": "CreateTerminal", "id": "half", "cwd": null });
    let mut out = bytes::BytesMut::new();
    Frame::new(
        Channel::Control,
        0,
        bytes::Bytes::from(serde_json::to_vec(&request).expect("encode")),
    )
    .encode(&mut out)
    .expect("encode");
    {
        let stdin = relay.stdin.as_mut().expect("stdin");
        stdin.write_all(&out[..1]).expect("write one byte");
        stdin.flush().expect("flush");
    }
    std::thread::sleep(Duration::from_millis(200));
    stop(&mut relay);

    // Give the daemon time to notice and clean up.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut after = machine.state_hash();
    while Instant::now() < deadline && after != before {
        std::thread::sleep(Duration::from_millis(200));
        after = machine.state_hash();
    }
    assert_eq!(
        before, after,
        "a disconnect in the middle of a command changed the source workspace"
    );

    // And the machine still works afterwards.
    let mut again = machine.relay();
    let welcome = handshake(&mut again).expect("the machine still answers");
    assert!(welcome.contains("codec"));
    stop(&mut again);
}

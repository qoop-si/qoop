//! Milestone 1 gates that need a real daemon.
//!
//! Each test names its gate. A gate that passes for a weaker reason than its name is worse
//! than no gate, so where an assertion could be satisfied by something other than the
//! behaviour under test, it asks the OS or asks the shell instead of asking us.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use qoro_runtime::daemon::{ResizeOutcome, arbitrate_resize};
use qoro_testkit::{PATIENCE, TestDaemon};

use qoro_domain::{ClientId, OwnershipEvent, PtySize, SizePolicy};

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { cols, rows }
}

/// G1.7: the owner's resize propagates and a second, smaller, non-owner client is refused.
#[tokio::test]
async fn g1_7_a_non_owner_cannot_resize_the_terminal() {
    let d = TestDaemon::start().await;
    let mut owner = d.attach("desktop").await;
    owner.resize(132, 43).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    owner.input(b"stty size\n").await;
    assert!(
        owner.wait_for_text("43 132", PATIENCE).await,
        "the owner's resize did not reach the child"
    );

    // A second client asks for a much smaller size. The child must not see it.
    let mut phone = d.attach("phone").await;
    phone.resize(50, 20).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Asserted as an absence. The first `stty size` output is still on the screen, so
    // "43 132 is there" is true whatever the size became, and this test passed with the
    // ownership rule removed. "20 50 is not there" is only true if the non-owner's size
    // never took.
    owner.forget_seen();
    owner.input(b"echo AFTER_$((6*7)); stty size\n").await;
    assert!(
        owner.wait_for_text("AFTER_42", PATIENCE).await,
        "the shell never answered"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner
        .wait_for_text("never matches", Duration::from_millis(200))
        .await;
    let screen = owner
        .screen(owner.default_stream())
        .expect("a screen")
        .lines
        .join("\n");
    assert!(
        !screen.contains("20 50"),
        "a non-owner resized the terminal, so a small client can reflow a large one:\n{screen}"
    );
}

/// The arbitration itself, without a PTY, including the case the gate above cannot show:
/// a silent owner keeps ownership.
#[test]
fn g1_7_arbitration_rules() {
    let desktop = ClientId::new("desktop");
    let phone = ClientId::new("phone");

    let mut policy = SizePolicy::Unowned;
    assert_eq!(
        arbitrate_resize(&mut policy, &desktop, true, size(132, 43)),
        ResizeOutcome::Applied,
        "the first writable client claims an unowned terminal"
    );
    assert_eq!(policy, SizePolicy::Owner(desktop.clone()));

    assert_eq!(
        arbitrate_resize(&mut policy, &phone, true, size(50, 20)),
        ResizeOutcome::NotOwner
    );
    // A silent owner keeps ownership: there is no event that expresses "did not answer".
    assert_eq!(policy, SizePolicy::Owner(desktop.clone()));

    // An observer is refused under any policy.
    assert_eq!(
        arbitrate_resize(&mut policy, &desktop, false, size(80, 24)),
        ResizeOutcome::Refused
    );

    // A size outside the supported range is refused even from the owner.
    assert_eq!(
        arbitrate_resize(&mut policy, &desktop, true, size(0, 24)),
        ResizeOutcome::Refused
    );

    // The owner leaving releases it, and the next writable client can claim it.
    policy = policy.apply(OwnershipEvent::OwnerDisconnected, None);
    assert_eq!(policy, SizePolicy::Unowned);
    assert_eq!(
        arbitrate_resize(&mut policy, &phone, true, size(50, 20)),
        ResizeOutcome::Applied
    );
}

/// G1.12: the socket is never accessible to group or other, at any instant.
#[tokio::test]
async fn g1_12_the_socket_is_never_group_or_world_accessible() {
    let d = TestDaemon::start().await;
    let meta = std::fs::metadata(d.socket()).expect("socket exists");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode & 0o077, 0, "socket mode is {mode:o}");
    let dir_mode = std::fs::metadata(d.dir()).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode & 0o077, 0, "runtime dir mode is {dir_mode:o}");
}

/// G1.6a: a large paste into a raw-mode child arrives byte for byte, in order.
///
/// The child is put in raw mode first, because the line discipline discards a payload with
/// no line terminator in canonical mode however well the daemon behaves. That limit is the
/// subject of the next test rather than a reason to weaken this one.
#[tokio::test]
async fn g1_6a_a_large_paste_into_a_raw_mode_child_arrives_intact() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("paster").await;

    let out = d.dir().join("paste.out");
    // `head -c`, not `cat`: in raw mode the end of transmission character is not special,
    // so sending one to close `cat`'s stdin delivers a literal byte instead, and the test
    // then either sees one byte too many or wins a race against the flush. Counting the
    // bytes at the child is the honest way to end the transfer.
    c.input(format!("stty raw; head -c {} > {}\n", 256 * 1024, out.display()).as_bytes())
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 256 KiB of a repeating pattern, so a reordering or a lost chunk is detectable rather
    // than merely a wrong length.
    let payload: Vec<u8> = (0..256 * 1024)
        .map(|i| b"0123456789abcdef"[i % 16])
        .collect();
    c.input(&payload).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut got = Vec::new();
    while std::time::Instant::now() < deadline {
        if let Ok(v) = std::fs::read(&out)
            && v.len() >= payload.len()
        {
            got = v;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        got.len(),
        payload.len(),
        "the child received {} of {} bytes",
        got.len(),
        payload.len()
    );
    assert_eq!(got, payload, "the bytes arrived out of order or corrupted");
}

/// G1.19 and invariant I14: a stalled client must not stop the terminal from tearing down.
///
/// An exiting child cannot complete until its tty output is drained, so a terminal task that
/// stops draining because it is waiting on a stalled client makes `exit` hang in the kernel.
#[tokio::test]
async fn g1_19_a_stalled_client_does_not_block_the_terminal() {
    let d = TestDaemon::start().await;
    // This client never reads. Its socket buffer fills and stays full.
    let _stalled = d.attach("stalled").await;
    let mut active = d.attach("active").await;

    // Generate enough output to fill any reasonable socket buffer for the stalled client.
    active
        .input(b"for i in 1 2 3 4 5; do seq 1 2000; done\n")
        .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // The active client must still be served.
    // The marker is computed by the shell rather than written literally. A literal one is
    // echoed back by the tty as it is typed, so the assertion would pass without the child
    // ever running: the screen would show the marker either way.
    active.input(b"echo RESPONSIVE_$((6*7))\n").await;
    assert!(
        active.wait_for_text("RESPONSIVE_42", PATIENCE).await,
        "a stalled client stopped the terminal from serving anyone else"
    );
}

/// G1.22: bounded parser state. An unterminated control string must not grow memory.
#[tokio::test]
async fn g1_22_an_unterminated_control_string_does_not_grow_memory() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("fuzz").await;
    // The threat is the child's output, not the client's input: a program that opens an OSC
    // and then writes without ever terminating it. Driving it from the child is also the
    // only way to feed it at all, because a line this long typed at a canonical-mode prompt
    // would hit the terminal's own line limit long before it reached the parser.
    c.input(
        b"printf '\\033]0;'; head -c 600000 /dev/zero | tr '\\0' A; printf '\\a'; echo ALIVE_$((6*7))\n",
    )
    .await;
    // Computed by the shell, so that the tty's echo of the command line cannot satisfy the
    // assertion on its own. With a literal marker this test passed in ten milliseconds
    // without the parser having recovered from anything.
    assert!(
        c.wait_for_text("ALIVE_42", PATIENCE).await,
        "the parser did not recover from an unterminated control string"
    );
}

/// G1.10: many simultaneous attaches all succeed and are all served.
#[tokio::test]
async fn g1_10_many_simultaneous_clients_all_attach() {
    let d = TestDaemon::start().await;
    let mut clients = Vec::new();
    for i in 0..32 {
        clients.push(d.attach(&format!("c{i}")).await);
    }
    // The first one drives, and every one of them must see the result, which is what proves
    // they are all really attached rather than merely connected.
    clients[0].input(b"echo ATTACHED_$((6*7))\n").await;
    for (i, c) in clients.iter_mut().enumerate() {
        assert!(
            c.wait_for_text("ATTACHED_42", PATIENCE).await,
            "client {i} of 32 was not served"
        );
    }
}

/// G1.16: a snapshot larger than one frame goes through the chunked path, and the client
/// that reassembles it ends up with exactly the daemon's grid.
///
/// The size is computed and then asserted, not assumed. A gate that picked 200x60 because
/// it looked big would silently stop testing chunking the day the codec got denser.
#[tokio::test]
async fn g1_16_a_snapshot_larger_than_one_frame_reassembles_exactly() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("render").await;
    // Chosen so that a full grid cannot fit in one frame: 400 by 200 is 80000 cells of
    // text before the JSON around it, against a 65536 byte frame. The assertion below is
    // what actually holds this true, so a denser codec turns this into a failing test
    // rather than a silently pointless one.
    c.resize(400, 200).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Fill the screen edge to edge. An empty grid serialises small however large it is,
    // and so does a wide grid whose rows are short: the first version of this test printed
    // 39 character rows into a 400 column terminal and produced a 3885 byte snapshot.
    c.input(b"for i in $(seq 1 400); do printf '%0399d\\n' $i; done\n")
        .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    c.input(b"echo FILLED_$((6*7))\n").await;
    let snapshot = c
        .wait_for(|s| s.lines.join("\n").contains("FILLED_42"), PATIENCE)
        .await
        .expect("the screen filled");

    let encoded = serde_json::to_vec(&snapshot).expect("encode");
    assert!(
        encoded.len() > qoro_proto::MAX_FRAME,
        "the snapshot is {} bytes, which fits in one frame, so this gate would not have \
         exercised the chunked path at all",
        encoded.len()
    );

    // A second client attaching now receives the same screen from the same daemon state.
    // Two independent reassemblies of the same grid is what proves the transfer, rather
    // than one client agreeing with itself.
    let mut fresh = d.attach("fresh").await;
    let other = fresh
        .wait_for(|s| s.lines.join("\n").contains("FILLED_42"), PATIENCE)
        .await
        .expect("the fresh client received the screen");
    assert_eq!(other.cols, snapshot.cols);
    assert_eq!(other.rows, snapshot.rows);
    assert_eq!(
        other.lines, snapshot.lines,
        "two clients reassembled different grids from the same daemon state"
    );
}

/// G1.17: a device status report reaches the child, not just the parser.
///
/// An application that asks where the cursor is blocks until it is told. If the reply were
/// only computed and dropped, everything would look right on screen and the child would
/// hang, which is the failure this gate exists for.
#[tokio::test]
async fn g1_17_a_cursor_position_request_is_answered() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("dsr").await;
    // `read -d R` stops at the R that terminates the report. Its exit status is the
    // assertion: non-zero means the reply never came.
    c.input(b"printf '\\033[6n'; read -t 3 -r -d R reply; echo DSR_$?_$((6*7))\n")
        .await;
    assert!(
        c.wait_for_text("DSR_0_42", PATIENCE).await,
        "the child never received a cursor position report"
    );
}

/// G1.18: a client that never types still acknowledges, so the daemon can tell a quiet
/// client from a wedged one.
#[tokio::test]
async fn g1_18_a_silent_client_still_acknowledges() {
    let d = TestDaemon::start().await;
    let mut driver = d.attach("driver").await;
    let mut silent = d.attach("silent").await;

    // Output the silent client will receive without ever sending anything itself.
    driver.input(b"seq 1 40\n").await;
    assert!(driver.wait_for_text("40", PATIENCE).await);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut acked = 0;
    while Instant::now() < deadline {
        silent.drain_incoming(Duration::from_millis(200)).await;
        silent.ack_if_due().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        acked = d
            .connections()
            .progress()
            .iter()
            .filter(|(_, _, acked)| *acked > 0)
            .count();
        if acked >= 1 {
            break;
        }
    }
    assert!(
        silent.renders_received() > 0,
        "the silent client never received a render, so this gate proved nothing"
    );
    assert!(
        acked >= 1,
        "the daemon never saw an acknowledgement from a client that only read"
    );
}

/// G1.21: at the connection budget, the connection that stopped reading is the one that
/// leaves, and an existing terminal stays responsive throughout.
#[tokio::test]
async fn g1_21_overload_evicts_by_drain_rate_not_by_buffered_size() {
    let d = TestDaemon::start().await;
    let mut active = d.attach("active").await;
    active.input(b"echo BEFORE_$((6*7))\n").await;
    assert!(active.wait_for_text("BEFORE_42", PATIENCE).await);

    // Fill the budget with connections that complete a handshake and then never read.
    let mut wedged = Vec::new();
    for i in 0..(qoro_runtime::daemon::MAX_CONNECTIONS - 1) {
        wedged.push(d.attach(&format!("wedged{i}")).await);
    }
    assert_eq!(d.connections().len(), qoro_runtime::daemon::MAX_CONNECTIONS);

    // The active client has to actually acknowledge before the newcomer arrives, or its
    // drain rate is zero like everyone else's and it is simply the first zero in the list.
    // Getting this wrong is how the first run of this test evicted the one client that was
    // reading.
    let ack_deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < ack_deadline {
        active.drain_incoming(Duration::from_millis(200)).await;
        active.ack_if_due().await;
        let acked_active = d
            .connections()
            .progress()
            .iter()
            .any(|(id, _, acked)| id.as_str() == "c1" && *acked > 0);
        if acked_active {
            break;
        }
    }
    assert!(
        d.connections()
            .progress()
            .iter()
            .any(|(id, _, acked)| id.as_str() == "c1" && *acked > 0),
        "the reading client never acknowledged, so drain rate cannot distinguish it"
    );

    // One more connection has to displace someone.
    let mut newcomer = d.attach("newcomer").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        d.connections().len(),
        qoro_runtime::daemon::MAX_CONNECTIONS,
        "the budget was exceeded rather than enforced"
    );
    let survivors: Vec<String> = d
        .connections()
        .progress()
        .iter()
        .map(|(id, _, _)| id.as_str().to_string())
        .collect();
    assert!(
        survivors.iter().any(|id| id == "c1"),
        "the client that was reading was evicted; survivors: {survivors:?}"
    );

    // The terminal is still serving: the active client, which never stopped reading, still
    // gets what it types.
    active.input(b"echo AFTER_$((6*7))\n").await;
    assert!(
        active.wait_for_text("AFTER_42", PATIENCE).await,
        "an existing terminal stopped being responsive under overload"
    );
    assert!(
        newcomer.wait_for_text("AFTER_42", PATIENCE).await,
        "the newly admitted client was not served"
    );
}

/// G1.6b: a paste with no line terminator into a canonical-mode child is reported, not
/// counted as delivered.
///
/// The measurement behind this gate: 1 MiB written to a canonical-mode tty with no newline
/// arrived as 0 bytes, and every `write` returned success. A gate asserting "every byte
/// arrives" would pass or fail on how the fixture happened to be shaped, which is why the
/// assertion is about the warning instead.
#[tokio::test]
async fn g1_6b_a_canonical_mode_paste_without_a_newline_is_reported() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("canonical").await;
    // An interactive shell at its prompt is NOT in canonical mode: readline turns ICANON
    // off to read a line itself. The canonical case is a foreground command that leaves the
    // line discipline alone, which is what the shell restores the terminal to before
    // running one. The first version of this gate pasted at the prompt and saw no warning
    // because there was correctly nothing to warn about.
    c.input(b"cat > /dev/null\n").await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    c.input(&vec![b'x'; 512 * 1024]).await;
    let deadline = Instant::now() + PATIENCE;
    let mut warnings = Vec::new();
    while Instant::now() < deadline {
        warnings = d.warnings();
        if !warnings.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        warnings.iter().any(|w| matches!(
            w,
            qoro_runtime::daemon::Warning::CanonicalPasteWithoutNewline { bytes, .. }
                if *bytes >= 512 * 1024
        )),
        "the daemon reported success for a paste the line discipline discards: {warnings:?}"
    );
}

/// The other half, which a positive test alone would let rot: a paste made of ordinary
/// lines is ordinary input and must not raise anything, however large it is.
#[tokio::test]
async fn g1_6b_a_terminated_paste_is_not_reported_as_a_problem() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("terminated").await;
    c.input(b"cat > /dev/null\n").await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Short lines, not one long line with a newline on the end. A single line longer than
    // the canonical limit is discarded whether or not it is terminated, so the earlier
    // version of this fixture asserted silence for a paste that is thrown away in full.
    let line = vec![b'#'; 200];
    let mut payload = Vec::new();
    while payload.len() < 512 * 1024 {
        payload.extend_from_slice(&line);
        payload.push(b'\n');
    }
    c.input(&payload).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        d.warnings().is_empty(),
        "a newline terminated paste was reported as discarded: {:?}",
        d.warnings()
    );
}

/// G1.13: a connection from another uid is closed before any protocol byte is sent.
///
/// CI cannot create a second uid, so the gate reports SKIPPED with a reason rather than
/// passing quietly. What it can always check is that the rule itself is not vacuous.
#[test]
fn g1_13_a_connection_from_another_uid_is_refused() {
    let ours = qoro_runtime::paths::current_uid();
    if ours != 0 {
        println!(
            "SKIPPED G1.13: creating a second uid needs root and this process runs as uid \
             {ours}. The check itself is asserted below."
        );
    }
    // Not vacuous: the daemon compares against our own uid, and no other uid is ours.
    assert!(qoro_runtime::daemon::peer_is_ours(ours));
    assert!(!qoro_runtime::daemon::peer_is_ours(ours.wrapping_add(1)));
    assert!(!qoro_runtime::daemon::peer_is_ours(0xffff_fffe));
}

/// G1.15: a child's last output survives its exit, whole and contiguous.
///
/// The mechanism this protects: an exiting child cannot complete until its tty output is
/// drained, so teardown has to keep reading the master. Measured: not reaped after four
/// seconds with two undrained bytes, reaped immediately after draining with the master
/// still open. Draining into a sink would satisfy the kernel and lose the output, which is
/// the failure this gate is shaped to catch.
#[tokio::test]
async fn g1_15_the_childs_last_output_survives_teardown() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("farewell").await;
    c.input(b"echo READY_$((6*7))\n").await;
    assert!(c.wait_for_text("READY_42", PATIENCE).await);

    // One long line, printed and then immediately exited. The length is what makes the
    // assertion about contiguity rather than presence: a second reader of the master would
    // take a slice out of the middle and the line would still "contain" the marker.
    let marker = "Z".repeat(60);
    c.input(format!("printf 'BYE_{marker}_END\\n'; exit\n").as_bytes())
        .await;

    let expected = format!("BYE_{marker}_END");
    let found = c
        .wait_for(
            move |s| s.lines.iter().any(|l| l.contains(&expected)),
            PATIENCE,
        )
        .await;
    assert!(
        found.is_some(),
        "the child's last line did not survive teardown whole"
    );
}

/// G1.14: containment reaches a job that escaped the process group, and stops at the
/// session boundary.
#[tokio::test]
async fn g1_14_teardown_contains_the_session_and_nothing_else() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("teardown").await;

    // A neighbour in a different session, started by this test rather than by the shell.
    // It must survive: containment is by controlling terminal, and this one has none.
    let mut neighbour = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn a neighbour");
    let neighbour_pid = neighbour.id() as i32;
    qoro_testkit::track_pid(neighbour_pid);

    // A job that leaves the shell's process group. `kill(-pgid)` cannot reach this, which
    // is the whole reason containment enumerates by controlling terminal instead.
    let pid_file = d.dir().join("escapee.pid");
    c.input(
        format!(
            "nohup sh -c 'echo $$ > {}; sleep 30' >/dev/null 2>&1 &\n",
            pid_file.display()
        )
        .as_bytes(),
    )
    .await;
    c.input(b"echo STARTED_$((6*7))\n").await;
    assert!(c.wait_for_text("STARTED_42", PATIENCE).await);

    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && !pid_file.exists() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let escapee: i32 = std::fs::read_to_string(&pid_file)
        .expect("the escapee wrote its pid")
        .trim()
        .parse()
        .expect("a numeric pid");
    qoro_testkit::track_pid(escapee);
    assert!(alive(escapee), "the escapee was never running");

    c.input(b"exit\n").await;

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && alive(escapee) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !alive(escapee),
        "a job in its own process group survived teardown, pid {escapee}"
    );
    assert!(
        alive(neighbour_pid),
        "teardown killed a process in a different session, pid {neighbour_pid}"
    );

    let _ = neighbour.kill();
    let _ = neighbour.wait();
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// G1.14, the assertion the mechanism cannot be trusted without: enumerating only after the
/// first signal finds nothing.
///
/// POSIX revokes the controlling terminal when the controlling process terminates. An
/// implementation that re-enumerated before each escalation step, which is what an earlier
/// version of ADR-014 said, would contain nothing from the second step onward. Nobody
/// predicted this; it was measured, and this test is what stops the design drifting back.
#[test]
fn g1_14_enumerating_after_the_leader_dies_finds_an_empty_set() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let pty = qoro_pty::sys::open_pty(80, 24).expect("pty");
    let tty_dev = qoro_pty::sys::tty_dev(pty.slave.as_raw_fd()).expect("tty dev");

    let slave = pty.slave.as_raw_fd();
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-i");
    // SAFETY: the closure runs post-fork and calls only async-signal-safe syscalls with no
    // allocation. This is the same session setup the daemon performs.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for target in 0..3 {
                if libc::dup2(slave, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn");
    drop(pty.slave);
    qoro_testkit::track_pid(child.id() as i32);
    std::thread::sleep(Duration::from_millis(500));

    let before = qoro_pty::sys::enumerate_tty(tty_dev);
    assert!(
        !before.is_empty(),
        "nothing was on the terminal before the leader died, so the comparison is vacuous"
    );

    // SAFETY: a signal to a child this test spawned.
    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    // Draining is not tidiness, it is what makes the child reapable. Measured: not reaped
    // after four seconds with two undrained bytes on the master, reaped immediately after
    // draining with the master still open. A `wait` here without this loop hangs, which is
    // exactly what the first version of this test did.
    let master = pty.master.as_raw_fd();
    qoro_pty::sys::set_nonblocking(master).expect("nonblocking");
    let mut sink = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // SAFETY: master is live and sink is a valid writable buffer.
        let _ = unsafe { libc::read(master, sink.as_mut_ptr() as *mut libc::c_void, sink.len()) };
        match child.try_wait() {
            Ok(Some(_)) => break,
            _ if Instant::now() > deadline => panic!("the child was never reaped"),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    std::thread::sleep(Duration::from_millis(300));

    let after = qoro_pty::sys::enumerate_tty(tty_dev);
    assert!(
        after.is_empty(),
        "the kernel kept the tty association after the leader died ({} members). If this \
         is now true, ADR-014 can be simplified, but not before someone checks why",
        after.len()
    );
}

/// G1.14, the session outlives the revoke: a job orphaned by the leader's death is still
/// found by its session id while the leader is unreaped.
///
/// The tty scan runs 50 ms after output, so `nohup job &` followed at once by `exit` was
/// never on any scan, and the revoke then hid it from every later one. Measured before
/// the session read existed: the cleanup gate's holder survived teardown every run.
#[test]
fn g1_14_a_job_orphaned_by_the_leader_is_found_by_its_session() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let pty = qoro_pty::sys::open_pty(80, 24).expect("pty");
    let tty_dev = qoro_pty::sys::tty_dev(pty.slave.as_raw_fd()).expect("tty dev");
    let pid_file = tempfile::NamedTempFile::new().expect("tempfile");

    let slave = pty.slave.as_raw_fd();
    let mut command = std::process::Command::new("/bin/sh");
    // `set -m` puts the job in its own group, like an interactive shell does, so only the
    // containment set can reach it.
    command.args([
        "-c",
        &format!(
            "set -m; sleep 30 >/dev/null 2>&1 & echo $! > {}; exec sleep 30",
            pid_file.path().display()
        ),
    ]);
    // SAFETY: the closure runs post-fork and calls only async-signal-safe syscalls with no
    // allocation. This is the same session setup the daemon performs.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for target in 0..3 {
                if libc::dup2(slave, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn");
    drop(pty.slave);
    let leader = child.id() as i32;
    qoro_testkit::track_pid(leader);

    let deadline = Instant::now() + Duration::from_secs(5);
    let job: i32 = loop {
        if let Ok(Ok(pid)) = std::fs::read_to_string(pid_file.path()).map(|s| s.trim().parse()) {
            break pid;
        }
        assert!(Instant::now() < deadline, "the job never wrote its pid");
        std::thread::sleep(Duration::from_millis(20));
    };
    qoro_testkit::track_pid(job);

    // SAFETY: a signal to a child this test spawned. Deliberately not reaped yet.
    unsafe { libc::kill(leader, libc::SIGKILL) };
    std::thread::sleep(Duration::from_millis(300));

    let on_tty = qoro_pty::sys::enumerate_tty(tty_dev);
    assert!(
        on_tty.iter().all(|m| m.pid.0 != job),
        "the job is still on the revoked tty, so this gate no longer proves anything"
    );
    let (session, _) = qoro_pty::sys::enumerate_session(qoro_pty::teardown::Pid(leader));
    let found = session.iter().any(|m| m.pid.0 == job);

    // SAFETY: signals to processes this test spawned.
    unsafe { libc::kill(job, libc::SIGKILL) };
    let _ = child.wait();
    assert!(
        found,
        "the orphaned job {job} is not in its dead leader's session"
    );
}

/// G1.14, enumerator completeness. A truncating enumerator would pass every containment
/// test by finding only recently created processes, which are exactly the ones a test
/// creates. An earlier version read a quarter of the process table, oldest first.
#[test]
fn g1_14_the_enumerator_sees_the_whole_process_table() {
    let scanned = qoro_pty::sys::scan_all_pids();
    let ps = std::process::Command::new("ps")
        .arg("-ax")
        .output()
        .expect("ps");
    // One header line.
    let visible = String::from_utf8_lossy(&ps.stdout).lines().count() - 1;
    let difference = scanned.len().abs_diff(visible);
    assert!(
        difference * 20 < visible,
        "the enumerator saw {} pids and ps sees {visible}",
        scanned.len()
    );
}

/// G1.2: a killed client leaves the shell running, and reattaching shows the same screen.
///
/// "Byte identical" is the assertion that matters. "The shell is still alive" would pass
/// with a fresh screen, which is the failure a user would actually notice.
#[tokio::test]
async fn g1_2_reattach_after_a_killed_client_shows_the_identical_grid() {
    let d = TestDaemon::start().await;
    let mut first = d.attach("doomed").await;
    first.input(b"echo MARK_$((6*7))\n").await;
    let before = first
        .wait_for(|s| s.lines.join("\n").contains("MARK_42"), PATIENCE)
        .await
        .expect("the marker appeared");

    // Drop the connection without any protocol goodbye, which is what a SIGKILLed client
    // looks like from the daemon's side.
    drop(first);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut second = d.attach("reattached").await;
    let after = second
        .wait_for(|s| s.lines.join("\n").contains("MARK_42"), PATIENCE)
        .await
        .expect("the reattached client received a screen");
    assert_eq!(after.cols, before.cols);
    assert_eq!(after.rows, before.rows);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(
        after.lines, before.lines,
        "the grid after reattaching is not the grid before the client died"
    );
}

/// The regression that made `TerminalUpdate::Ended` carry a snapshot: a coalescing render
/// lane let the end of a terminal overwrite its last screen before any client saw it.
///
/// Kept separate from G1.15 because G1.15 is about draining the pty and this is about the
/// channel between the terminal and the client. Both have to hold for the child's last
/// output to reach a person, and each passed while the other was broken.
#[tokio::test]
async fn the_end_of_a_terminal_never_overwrites_its_last_screen() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("last-screen").await;
    c.input(b"echo READY_$((6*7))\n").await;
    assert!(c.wait_for_text("READY_42", PATIENCE).await);

    // Printed and exited in the same command, so the final snapshot and the end of the
    // terminal are as close together as a shell can put them.
    c.input(b"printf 'FINAL_%s\\n' $((6*7)); exit\n").await;
    assert!(
        c.wait_for_text("FINAL_42", PATIENCE).await,
        "the terminal ended and took its last screen with it"
    );
}

/// G1.6b, the case the first version of the warning missed entirely: one line longer than
/// the canonical limit, ending in a newline.
///
/// Measured against a real pty with `cat` on the far side: 524288 bytes written as one
/// terminated line, 0 received, every write returning success. A warning that fires only
/// when there is no newline anywhere is silent for exactly the paste a person is most
/// likely to make.
#[tokio::test]
async fn g1_6b_a_single_line_longer_than_the_canonical_limit_is_reported() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("long-line").await;
    c.input(b"cat > /dev/null\n").await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let mut payload = vec![b'x'; 512 * 1024];
    payload.push(b'\n');
    c.input(&payload).await;

    let deadline = Instant::now() + PATIENCE;
    let mut warnings = Vec::new();
    while Instant::now() < deadline {
        warnings = d.warnings();
        if !warnings.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        warnings.iter().any(|w| matches!(
            w,
            qoro_runtime::daemon::Warning::CanonicalPasteWithoutNewline { bytes, .. }
                if *bytes >= 512 * 1024
        )),
        "a single line past the canonical limit was reported as delivered: {warnings:?}"
    );
}

/// G1.21, the half the count assertion cannot see: ending a connection really closes its
/// socket.
///
/// Removing a connection from the registry and aborting its task leaves the writer running
/// and holding the only write half, so the socket stays open and the peer keeps receiving.
/// The registry then reports a number that does not describe what the process is holding,
/// and the next admission evicts another victim on top of the leak. Every count assertion
/// stays green through all of that, which is why this asserts the socket instead.
///
/// It calls `disconnect` rather than arranging an eviction because the policy chooses which
/// connection to end, and this is about what ending one does.
#[tokio::test]
async fn ending_a_connection_really_closes_its_socket() {
    use tokio::io::AsyncReadExt;

    let d = TestDaemon::start().await;
    let (victim, _) = qoro_runtime::client::connect(d.socket(), "victim")
        .await
        .expect("connect");
    let (mut victim_rd, _victim_wr) = victim.into_split();

    // Wait until the daemon has registered it, so the name below exists.
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && d.connections().is_empty() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let name = d.connections().progress()[0].0.clone();
    d.connections().disconnect(&name);

    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), victim_rd.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => {
                closed = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Err(_) => continue,
        }
    }
    assert!(
        closed,
        "the connection was removed from the registry but its socket stayed open, so the \
         count does not describe what the daemon is holding"
    );
}

/// When the owner leaves, a client that asked for a size and was refused gets the geometry
/// and its size at once, without asking again (ADR-007: ownership passes to the
/// longest-attached writable client). A second window otherwise sat at the first one's
/// size after it closed, until something happened to resize again.
#[tokio::test]
async fn a_refused_size_takes_effect_when_the_owner_leaves() {
    let d = TestDaemon::start().await;
    let mut desktop = d.attach("desktop").await;
    desktop.resize(132, 43).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut laptop = d.attach("laptop").await;
    laptop.resize(90, 30).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    laptop.input(b"stty size\n").await;
    assert!(
        laptop.wait_for_text("43 132", PATIENCE).await,
        "the refused size took effect while the owner was there"
    );

    drop(desktop);
    tokio::time::sleep(Duration::from_millis(600)).await;
    laptop.forget_seen();
    laptop.input(b"stty size\n").await;
    assert!(
        laptop.wait_for_text("30 90", PATIENCE).await,
        "the waiting client's size never took after the owner left"
    );
}

/// The size owner releasing ownership must not depend on how its connection ended.
///
/// The cleanup used to sit after the read loop and be reachable only by the `break` paths,
/// so a single malformed frame returned early and left `SizePolicy::Owner` pointing at a
/// connection that no longer existed. Every later client's resize was then refused for the
/// life of the terminal. The owner is the one that ends badly here, because an error on any
/// other connection would not have shown it.
#[tokio::test]
async fn an_owner_that_ends_on_an_error_still_releases_ownership() {
    use tokio::io::AsyncWriteExt;

    let d = TestDaemon::start().await;
    let (owner, _) = qoro_runtime::client::connect(d.socket(), "owner")
        .await
        .expect("connect");
    let (_owner_rd, mut owner_wr) = owner.into_split();

    // Become the size owner.
    let mut seq = 0u32;
    let resize = qoro_runtime::client::encode_command_to(
        1,
        &qoro_runtime::daemon::ClientWire::Resize {
            cols: 120,
            rows: 40,
        },
        &mut seq,
    )
    .expect("encode");
    owner_wr.write_all(&resize).await.expect("write");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let mut witness = d.attach("witness").await;
    witness.input(b"stty size\n").await;
    assert!(
        witness.wait_for_text("40 120", PATIENCE).await,
        "the owner's resize never reached the child, so this proves nothing"
    );

    // A frame with a reserved flag bit set. The decoder refuses it, which is correct, and
    // the owner's connection ends on an error path rather than on a clean break.
    let mut bad = bytes::BytesMut::new();
    bad.extend_from_slice(&6u32.to_le_bytes());
    bad.extend_from_slice(&[3u8, 0b1111_1111u8]);
    bad.extend_from_slice(&1u32.to_le_bytes());
    bad.extend_from_slice(&0u32.to_le_bytes());
    owner_wr.write_all(&bad).await.expect("write");
    tokio::time::sleep(Duration::from_millis(600)).await;

    // The witness, which is writable and attached, must now be able to take the geometry.
    witness.resize(90, 30).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    witness.input(b"stty size\n").await;
    assert!(
        witness.wait_for_text("30 90", PATIENCE).await,
        "ownership was stranded by a connection that ended on an error, so no later client \
         can ever resize this terminal"
    );
}

/// G1.14, the timing half: the grace window between escalation steps is actually waited
/// out.
///
/// A pty master whose slave descriptors are all closed is permanently readable and every
/// read returns zero, which is exactly the state teardown is entered from. A drain that
/// returned on end of stream collapsed the whole escalation into under two milliseconds:
/// measured, a job's SIGTERM handler was killed 372 microseconds into its cleanup, so
/// anything that flushes state on SIGTERM was killed mid flush on every terminal close.
#[tokio::test]
async fn g1_14_a_job_that_handles_sigterm_gets_its_grace_window() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("grace").await;
    let evidence = d.dir().join("handler.txt");

    // A job in its own process group that traps SIGTERM, takes 50 ms to clean up, and
    // records both ends of that. It ignores SIGHUP so that the escalation actually reaches
    // the second step, which is the one whose window this measures.
    //
    // Written to a file rather than typed as a one liner: the trap body has to survive one
    // level of shell quoting on the way in, and the first version silently lost the `start`
    // half to it, which looked exactly like the bug under test.
    let script = d.dir().join("job.sh");
    std::fs::write(
        &script,
        format!(
            "trap '' HUP\n\
             trap 'echo start >> {0}; sleep 0.05; echo done >> {0}; exit 0' TERM\n\
             while true; do sleep 0.02; done\n",
            evidence.display()
        ),
    )
    .expect("write the job");

    c.input(format!("nohup sh {} >/dev/null 2>&1 &\n", script.display()).as_bytes())
        .await;
    c.input(b"echo STARTED_$((6*7))\n").await;
    assert!(c.wait_for_text("STARTED_42", PATIENCE).await);
    tokio::time::sleep(Duration::from_millis(300)).await;

    c.input(b"exit\n").await;

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut text = String::new();
    while Instant::now() < deadline {
        text = std::fs::read_to_string(&evidence).unwrap_or_default();
        if text.contains("done") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        text.contains("start"),
        "the job never received SIGTERM at all, so this proves nothing: {text:?}"
    );
    assert!(
        text.contains("done"),
        "the job's SIGTERM handler was killed mid cleanup, so the grace window collapsed: \
         {text:?}"
    );
}

/// ADR-014 step 4, on the path that has no other route to the screen: a terminal closed on
/// request prints its farewell during teardown and nowhere else.
///
/// G1.15 passes by ordering rather than by design: there the child writes and then exits, so
/// the write is read by the ordinary loop before the next read reports end of stream. A
/// close while the child is alive has no such luck, and the drained bytes have to be fed to
/// the engine or the last thing the child said is lost.
#[tokio::test]
async fn output_produced_during_teardown_reaches_the_screen() {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let d = TestDaemon::start().await;
    let mut c = d.attach("farewell-on-close").await;
    let terminal = d.terminals().streams()[0];

    // A shell that says something on the way out, and only on the way out.
    c.input(b"trap 'echo GOODBYE_$((6*7))' EXIT\n").await;
    c.input(b"echo ARMED_$((6*8))\n").await;
    assert!(c.wait_for_text("ARMED_48", PATIENCE).await);

    let response = c
        .control(
            &ControlRequest::CloseTerminal {
                id: "close".into(),
                terminal,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(matches!(response, ControlResponse::TerminalClosed { .. }));

    assert!(
        c.wait_for_text_on(terminal, "GOODBYE_42", PATIENCE).await,
        "what the child printed during teardown was drained into a sink and lost"
    );
}

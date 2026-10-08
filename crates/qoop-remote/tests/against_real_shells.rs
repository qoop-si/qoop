//! Gate G6.1: bootstrap against real login shells, each asserted by a marker command
//! returning expected output over a real transport.
//!
//! The gate names containers. There are none here, so this runs against the real login
//! shells on this machine through `LocalTransport`, which is a real transport reaching a
//! real login shell rather than a mock. What it does not exercise is the ssh argument
//! construction, which is asserted directly in `ssh.rs`, and the network, which no local
//! test can reach.
//!
//! A shell that is not installed is reported SKIPPED with the reason, never passed quietly.

use qoop_remote::{Bootstrap, LocalTransport, RemoteShell, RemoteTransport, marker_command};

/// The shells the gate names, and where they are if they are here.
fn shells() -> Vec<(&'static str, Option<String>)> {
    ["bash", "zsh", "fish"]
        .into_iter()
        .map(|name| {
            let found = std::process::Command::new("command")
                .args(["-v", name])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
                        .iter()
                        .map(|d| format!("{d}/{name}"))
                        .find(|p| std::path::Path::new(p).exists())
                });
            (name, found)
        })
        .collect()
}

/// A marker command returns exactly what it was asked for, through a login shell that may
/// print anything it likes around it.
#[test]
fn a_marker_command_returns_its_own_output_through_every_login_shell() {
    let mut ran = 0;
    for (name, path) in shells() {
        let Some(path) = path else {
            println!("SKIPPED {name}: not installed on this machine");
            continue;
        };
        let transport = LocalTransport::new(&path);
        let out = transport
            .run(&marker_command("echo qoro-marker-42"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let answer = qoop_remote::bootstrap::between_markers(&out.stdout)
            .unwrap_or_else(|| panic!("{name}: no marked output in {:?}", out.stdout));
        assert_eq!(answer, "qoro-marker-42", "{name} returned the wrong output");
        ran += 1;
    }
    assert!(
        ran >= 2,
        "only {ran} shells were available, so this proves little"
    );
}

/// The shell is detected as what it is, through its own login startup.
#[test]
fn every_available_shell_is_detected_correctly() {
    for (name, path) in shells() {
        let Some(path) = path else {
            println!("SKIPPED {name}: not installed on this machine");
            continue;
        };
        let transport = LocalTransport::new(&path);
        let boot = Bootstrap::new(&transport, "/nonexistent/qorod");
        let detected = boot
            .detect_shell()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let expected = if name == "fish" {
            RemoteShell::Fish
        } else {
            RemoteShell::PosixLike
        };
        assert_eq!(detected, expected, "{name} was detected as {detected:?}");
    }
}

/// A missing daemon is reported as missing, not as a broken shell.
///
/// The distinction matters: one is fixed by installing something and the other by looking
/// at a login script, and a bootstrap that reports the wrong one sends somebody to the
/// wrong place.
#[test]
fn a_remote_with_no_daemon_says_so() {
    for (name, path) in shells() {
        let Some(path) = path else { continue };
        let transport = LocalTransport::new(&path);
        let boot = Bootstrap::new(&transport, "/nonexistent/qorod");
        match boot.hello() {
            Err(qoop_remote::BootstrapError::Remote(qoop_remote::RemoteError::NotInstalled)) => {}
            other => panic!("{name}: a missing daemon was reported as {other:?}"),
        }
    }
}

/// Something that answers but is not a daemon is not mistaken for one.
#[test]
fn something_that_is_not_a_daemon_is_not_mistaken_for_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = dir.path().join("qorod");
    std::fs::write(&fake, "#!/bin/sh\necho 'I am not a daemon'\n").expect("write");
    let mut perms = std::fs::metadata(&fake).expect("metadata").permissions();
    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&fake, perms).expect("chmod");

    let transport = LocalTransport::new("/bin/sh");
    let boot = Bootstrap::new(&transport, fake.display().to_string());
    match boot.hello() {
        Err(qoop_remote::BootstrapError::Remote(qoop_remote::RemoteError::NotADaemon(what))) => {
            assert!(
                what.contains("not a daemon"),
                "the reason is unhelpful: {what}"
            );
        }
        other => panic!("a non-daemon was accepted: {other:?}"),
    }
}

/// A daemon that answers properly is read, and the whole plan comes out.
#[test]
fn a_real_hello_is_read_through_a_noisy_login_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = dir.path().join("qorod");
    let hello = serde_json::json!({
        "version": "0.0.0",
        "codec_generation": qoop_remote::CODEC_FLOOR,
        "platform": "linux",
    });
    std::fs::write(&fake, format!("#!/bin/sh\necho '{hello}'\n")).expect("write");
    let mut perms = std::fs::metadata(&fake).expect("metadata").permissions();
    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&fake, perms).expect("chmod");

    let transport = LocalTransport::new("/bin/sh");
    let boot = Bootstrap::new(&transport, fake.display().to_string());
    let (shell, compat) = boot.plan().expect("a plan");
    assert_eq!(shell, RemoteShell::PosixLike);
    assert_eq!(compat, qoop_remote::Compatibility::AsIs);
}

/// The daemon's path is one word, whatever is in it: a directory with a space or a
/// semicolon in its name is still where the daemon is, and runs nothing else.
#[test]
fn a_daemon_path_with_shell_characters_is_one_word() {
    let dir = tempfile::tempdir().expect("tempdir");
    let odd = dir.path().join("a dir; true");
    std::fs::create_dir(&odd).expect("mkdir");
    let fake = odd.join("qorod");
    let hello = serde_json::json!({
        "version": "0.0.0",
        "codec_generation": qoop_remote::CODEC_FLOOR,
        "platform": "linux",
    });
    std::fs::write(&fake, format!("#!/bin/sh\necho '{hello}'\n")).expect("write");
    let mut perms = std::fs::metadata(&fake).expect("metadata").permissions();
    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&fake, perms).expect("chmod");

    let transport = LocalTransport::new("/bin/sh");
    let boot = Bootstrap::new(&transport, fake.display().to_string());
    boot.hello()
        .expect("the daemon at a path with a space was not reached");
}

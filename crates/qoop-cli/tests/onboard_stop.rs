//! Ctrl-C in the onboarding wizard stops it where it is: the next step is said, the terminal
//! is left as it was found, nothing more is asked or started (no sign-in, no qass on the agent
//! that is installed), and the run exits 130.

#[cfg(target_os = "macos")]
#[test]
fn ctrl_c_in_onboarding_says_the_next_step_and_exits_130() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("a directory");
    let at = |name: &str| dir.path().join(name);
    // An installed agent qass would start on, which writes down that it ran.
    std::fs::create_dir(at("bin")).expect("bin");
    let stub = at("bin").join("claude");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\ntouch '{}'\n", at("ran").display()),
    )
    .expect("stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("mode");
    // `script` gives the wizard a terminal of its own; the shell around it reads the terminal's
    // settings before and after, and the exit code. The byte 0x03 reaches the wizard as Ctrl-C.
    let shell = format!(
        "stty -g > '{b}'; '{q}' onboard; echo $? > '{c}'; stty -g > '{a}'",
        b = at("before").display(),
        q = env!("CARGO_BIN_EXE_qoop"),
        c = at("code").display(),
        a = at("after").display(),
    );
    let mut child = qoop_testkit::sandboxed("/usr/bin/script")
        .args(["-q", "/dev/null", "/bin/sh", "-c", &shell])
        .env("TERM", "xterm-256color")
        .env("PATH", format!("{}:/usr/bin:/bin", at("bin").display()))
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(at("out")).expect("a log"))
        .stderr(Stdio::null())
        .spawn()
        .expect("script runs");
    let said = || std::fs::read_to_string(at("out")).unwrap_or_default();
    // The wizard is up once its first step is drawn.
    let deadline = Instant::now() + qoop_testkit::PATIENCE;
    while !said().contains("qoop onboard") {
        assert!(
            Instant::now() < deadline,
            "the wizard never drew: {}",
            said()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(b"\x03").expect("ctrl-c");
    while child.try_wait().expect("a status").is_none() {
        assert!(
            Instant::now() < deadline,
            "onboarding did not stop: {}",
            said()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(stdin);
    let said = said();
    let read = |name: &str| std::fs::read_to_string(at(name)).unwrap_or_default();
    assert_eq!(read("code").trim(), "130", "{said}");
    assert!(!read("before").is_empty());
    assert_eq!(
        read("before"),
        read("after"),
        "the terminal as it was found"
    );
    assert!(
        said.contains("onboarding stopped; run `qoop onboard` to finish"),
        "{said}"
    );
    assert!(!said.contains("Sign in"), "{said}");
    assert!(
        !at("ran").exists(),
        "qass was started on the installed agent"
    );
}

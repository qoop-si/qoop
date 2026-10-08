//! Gate G5.1, the half a round trip through a struct cannot show: the capture is asserted
//! against the child's real process view.
//!
//! A round trip test proves the struct is symmetrical. It does not prove that what was
//! captured is what the operating system actually gave the child, and that is the failure
//! of a design where argv is persisted, displayed, and never used.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use qoop_domain::LaunchIntent;
use qoop_setup::{Origin, Setup, capture_from};

/// Start a child from a Setup and ask the operating system what it actually got.
fn what_the_child_really_got(setup: &Setup) -> (Vec<String>, String, String) {
    let intent = setup.to_launch_intent().expect("a launch intent");
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("view.txt");

    // The child reports its own argv, cwd and one environment variable, read from its own
    // process rather than from anything we told it.
    let mut command = Command::new(intent.executable());
    command
        .args(&intent.argv()[1..])
        .current_dir(intent.cwd())
        .env_clear()
        .env("REPORT_TO", &out);
    for (name, value) in &intent.env.overrides {
        command.env(name, value);
    }
    let status = command.status().expect("spawn");
    assert!(status.success(), "the child failed: {status}");

    let text = std::fs::read_to_string(&out).expect("the child reported");
    let mut lines = text.lines();
    let argv: Vec<String> = lines
        .next()
        .expect("argv line")
        .split('\u{1}')
        .map(str::to_string)
        .collect();
    let cwd = lines.next().expect("cwd line").to_string();
    let override_seen = lines.next().unwrap_or_default().to_string();
    (argv, cwd, override_seen)
}

fn reporter_script(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("report.sh");
    std::fs::write(
        &path,
        // Reports what this process actually received: its own arguments, its own working
        // directory, and one variable from its own environment.
        "#!/bin/sh\n\
         printf '%s' \"$0\" > \"$REPORT_TO\"\n\
         for a in \"$@\"; do printf '\\001%s' \"$a\" >> \"$REPORT_TO\"; done\n\
         printf '\\n%s\\n%s\\n' \"$(pwd)\" \"$NODE_OPTIONS\" >> \"$REPORT_TO\"\n",
    )
    .expect("write");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

#[test]
fn what_was_captured_is_what_the_child_receives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = reporter_script(dir.path());
    let workdir = dir.path().join("work");
    std::fs::create_dir(&workdir).expect("mkdir");

    let mut intent = LaunchIntent::new(
        script.clone(),
        vec![
            OsString::from(script.display().to_string()),
            OsString::from("--plain"),
            OsString::from("with space"),
            OsString::from("--empty="),
        ],
        std::fs::canonicalize(&workdir).expect("canonicalize"),
    )
    .expect("intent");
    intent.env.overrides = vec![("NODE_OPTIONS".into(), "--require /tmp/x.js".into())];

    let setup = capture_from("reporter", &intent, Origin::default()).expect("capture");
    let restored = Setup::from_toml(&setup.to_toml().expect("encode")).expect("decode");

    let (argv, cwd, override_seen) = what_the_child_really_got(&restored);
    assert_eq!(
        argv[1..].to_vec(),
        vec![
            "--plain".to_string(),
            "with space".to_string(),
            "--empty=".to_string()
        ],
        "the child's own argv is not what was captured"
    );
    assert_eq!(
        cwd,
        std::fs::canonicalize(&workdir)
            .expect("canonicalize")
            .display()
            .to_string(),
        "the child's own working directory is not what was captured"
    );
    assert_eq!(
        override_seen, "--require /tmp/x.js",
        "the child's own environment is not what was captured"
    );
}

/// An argument that is not valid UTF-8 reaches the child byte for byte.
///
/// This is the case a capture that stores strings gets wrong silently: the round trip
/// succeeds, the child receives replacement characters, and nothing says so.
#[test]
fn an_argument_that_is_not_text_reaches_the_child_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workdir = dir.path().join("work");
    std::fs::create_dir(&workdir).expect("mkdir");
    let out = dir.path().join("raw.bin");

    // The child writes its second argument out as bytes.
    let script = dir.path().join("raw.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\" > \"$REPORT_TO\"\n").expect("write");
    let mut perms = std::fs::metadata(&script).expect("metadata").permissions();
    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&script, perms).expect("chmod");

    let raw = vec![0x2d, 0x2d, 0xff, 0xfe, 0x41];
    let intent = LaunchIntent::new(
        script.clone(),
        vec![
            OsString::from(script.display().to_string()),
            <OsString as std::os::unix::ffi::OsStringExt>::from_vec(raw.clone()),
        ],
        std::fs::canonicalize(&workdir).expect("canonicalize"),
    )
    .expect("intent");

    let setup = capture_from("raw", &intent, Origin::default()).expect("capture");
    let restored = Setup::from_toml(&setup.to_toml().expect("encode")).expect("decode");
    let restored_intent = restored.to_launch_intent().expect("restore");
    assert_eq!(
        restored_intent.argv()[1].as_bytes(),
        raw.as_slice(),
        "the bytes did not survive the artifact"
    );

    let status = Command::new(restored_intent.executable())
        .args(&restored_intent.argv()[1..])
        .current_dir(restored_intent.cwd())
        .env("REPORT_TO", &out)
        .status()
        .expect("spawn");
    assert!(status.success());
    assert_eq!(
        std::fs::read(&out).expect("read"),
        raw,
        "the child received different bytes from the ones that were captured"
    );
}

/// Restoring an agent terminal reattaches its native session rather than starting a new one
/// with the same argv.
#[test]
fn restoring_carries_the_native_session_and_not_only_the_argv() {
    let mut intent = LaunchIntent::new(
        "/bin/echo",
        vec![OsString::from("echo"), OsString::from("hi")],
        "/tmp",
    )
    .expect("intent");
    intent.native_session_ref = Some(qoop_domain::NativeSessionRef {
        kind: "claude".into(),
        value: "sess-abc".into(),
    });

    let setup = capture_from("agent", &intent, Origin::default()).expect("capture");
    let text = setup.to_toml().expect("encode");
    assert!(
        text.contains("sess-abc"),
        "the session reference was not captured, so restore would start a new session"
    );
    let restored = Setup::from_toml(&text)
        .expect("decode")
        .to_launch_intent()
        .expect("restore");
    assert_eq!(restored.native_session_ref, intent.native_session_ref);
}

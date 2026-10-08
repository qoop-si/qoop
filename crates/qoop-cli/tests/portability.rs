//! Gate G6.5: capture on one machine, trust the hash on another, run it there.
//!
//! Two machines are two daemon state directories here rather than two hosts. What that
//! leaves out is the network and the remote's login shell, both of which are covered
//! separately in `qoop-remote`. What it covers is the portability claim itself: the same
//! artifact, a machine that has never seen it, a person confirming it, and the workspace
//! coming up with the same executable, argv, cwd, env references and native session.
//!
//! Written as a gate rather than as an acceptance script nobody has to run.

use std::process::Command;

use qoop_setup::{NativeSession, Origin, Setup};

fn qoro() -> &'static str {
    env!("CARGO_BIN_EXE_qoop")
}

/// A capture as machine A would write it.
fn captured_on_a(work: &std::path::Path) -> Setup {
    Setup {
        name: "payments".into(),
        origin: Origin {
            machine: "machine-a".into(),
            user: "alice".into(),
            captured_at_unix: 1_700_000_000,
        },
        executable: "/bin/echo".into(),
        argv: vec!["echo".into(), "restored".into()],
        argv_bytes: Vec::new(),
        cwd: work.display().to_string(),
        inherit: vec!["HOME".into(), "PATH".into()],
        overrides: vec![("NODE_OPTIONS".into(), "--enable-source-maps".into())],
        env_refs: vec![qoop_setup::EnvRefEntry {
            name: "STRIPE_ACCOUNT".into(),
            required: false,
        }],
        secret_refs: vec![qoop_setup::SecretRefEntry {
            name: "STRIPE_KEY".into(),
            provider: "keychain".into(),
            required: true,
        }],
        services: Vec::new(),
        runtimes: Vec::new(),
        native_session: Some(NativeSession {
            kind: "claude".into(),
            value: "sess-from-a".into(),
        }),
    }
}

fn run(dir: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    answering(dir, args, "")
}

/// The same, with what a person types on stdin.
fn answering(dir: &std::path::Path, args: &[&str], typed: &str) -> (String, String, bool) {
    use std::io::Write;
    let mut child = Command::new(qoro())
        .args(args)
        .env("QORO_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run qoro");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(typed.as_bytes())
        .expect("type");
    let out = child.wait_with_output().expect("run qoro");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn a_setup_captured_on_one_machine_runs_on_another_after_it_is_trusted() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    for dir in [a.path(), b.path()] {
        std::fs::set_permissions(
            dir,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("chmod");
    }
    let work = a.path().join("code");
    std::fs::create_dir(&work).expect("mkdir");

    let setup = captured_on_a(&work);
    let artifact = a.path().join("payments.toml");
    std::fs::write(&artifact, setup.to_toml().expect("encode")).expect("write");
    let artifact = artifact.display().to_string();

    // Machine B has never seen it, so it may not run.
    let (_, _, ok) = run(b.path(), &["setup", "check", &artifact]);
    assert!(
        !ok,
        "an unseen Setup was admitted on a machine that never confirmed it"
    );

    // What B is shown before it decides.
    let (shown, _, ok) = run(b.path(), &["setup", "show", &artifact]);
    assert!(ok);
    for expected in [
        "/bin/echo",
        "NODE_OPTIONS",
        "STRIPE_KEY",
        "machine-a",
        "unverified",
    ] {
        assert!(
            shown.contains(expected),
            "the prompt does not show {expected}:\n{shown}"
        );
    }
    assert!(
        !shown.contains("sess-from-a") || shown.contains("unverified"),
        "the prompt is missing its origin caveat"
    );
    assert!(
        !shown.contains("[y/N]"),
        "`setup show` asks a question it never reads the answer to:\n{shown}"
    );

    // A person who says no, or says nothing, has not trusted it.
    for typed in ["n\n", ""] {
        let (asked, _, ok) = answering(b.path(), &["setup", "trust", &artifact], typed);
        assert!(!ok, "{typed:?} trusted the Setup");
        assert!(asked.contains("[y/N]"), "trust never asked:\n{asked}");
        let (_, _, ok) = run(b.path(), &["setup", "check", &artifact]);
        assert!(!ok, "{typed:?} was recorded as a confirmation");
    }

    // A person confirms it on B.
    let (_, _, ok) = answering(b.path(), &["setup", "trust", &artifact], "y\n");
    assert!(ok, "trusting the Setup failed");

    let (_, _, ok) = run(b.path(), &["setup", "check", &artifact]);
    assert!(ok, "a confirmed Setup is still refused");

    // And A, which never confirmed it, still refuses. Confirmation is per machine.
    let (_, _, ok) = run(a.path(), &["setup", "check", &artifact]);
    assert!(
        !ok,
        "confirming on one machine confirmed it on another, so the ledger is not local"
    );

    // What comes up on B is what was captured on A.
    let restored = Setup::from_toml(&std::fs::read_to_string(&artifact).expect("read"))
        .expect("decode")
        .to_launch_intent()
        .expect("restore");
    assert_eq!(restored.executable(), std::path::Path::new("/bin/echo"));
    assert_eq!(restored.argv()[1], "restored");
    assert_eq!(restored.cwd(), work.as_path());
    assert_eq!(restored.env.overrides[0].0, "NODE_OPTIONS");
    assert_eq!(restored.env.secret_refs[0].name, "STRIPE_KEY");
    assert_eq!(
        restored
            .native_session_ref
            .as_ref()
            .map(|s| s.value.as_str()),
        Some("sess-from-a"),
        "the agent terminal would start a new session instead of reattaching"
    );
}

/// Editing the artifact after it was trusted invalidates the confirmation.
#[test]
fn an_edited_setup_is_not_the_setup_that_was_trusted() {
    let b = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        b.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    let work = b.path().join("code");
    std::fs::create_dir(&work).expect("mkdir");

    let mut setup = captured_on_a(&work);
    let artifact = b.path().join("payments.toml");
    std::fs::write(&artifact, setup.to_toml().expect("encode")).expect("write");
    let artifact_path = artifact.display().to_string();

    let (_, _, ok) = answering(b.path(), &["setup", "trust", &artifact_path], "y\n");
    assert!(ok);
    let (_, _, ok) = run(b.path(), &["setup", "check", &artifact_path]);
    assert!(ok);

    // One argument more, which is how this goes wrong in practice.
    setup.argv.push("--dangerously-skip-permissions".into());
    std::fs::write(&artifact, setup.to_toml().expect("encode")).expect("write");
    let (_, _, ok) = run(b.path(), &["setup", "check", &artifact_path]);
    assert!(
        !ok,
        "an edited Setup ran on a confirmation given for a different one"
    );
}

/// G6.4 at the binary's edge: `--hello` answers with the codec generation, which is what
/// compatibility is decided on.
#[test]
fn the_binary_answers_a_bootstrap_with_its_codec_generation() {
    let out = Command::new(qoro()).arg("--hello").output().expect("run");
    assert!(out.status.success());
    let hello: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the answer is one line of json");
    assert_eq!(
        hello["codec_generation"].as_u64(),
        Some(u64::from(qoop_proto::CODEC_GENERATION))
    );
    assert!(hello["version"].is_string());
    assert!(hello["platform"].is_string());
}

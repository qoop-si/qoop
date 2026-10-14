//! qoop run as `arecord`: Claude Code's version check passes, what is not Claude Code's
//! recording goes to the real `arecord` further along PATH (once, never back and forth),
//! and with nothing to lend a microphone the answer is a failure, never silence.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

/// `arecord` with `args`, as a link to this build of qoop in `dir`, PATH set to `path`.
fn arecord(dir: &Path, path: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let link = dir.join("arecord");
    if !link.exists() {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qoop"), &link).unwrap();
    }
    let mut command = qoop_testkit::sandboxed(&link);
    command.args(args).env("PATH", path);
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("ran")
}

/// A stand-in for a machine's own `arecord`, saying what it was given.
fn real_arecord(dir: &Path) {
    let real = dir.join("arecord");
    std::fs::write(
        &real,
        "#!/bin/sh\necho \"real arecord $* handed=$QOOP_ARECORD_HANDED\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn claude_codes_version_check_passes() {
    let dir = tempfile::tempdir().unwrap();
    let out = arecord(dir.path(), "/usr/bin:/bin", &["--version"], &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("qoop"));
}

#[test]
fn a_persons_own_recording_goes_to_the_real_arecord_once() {
    let dir = tempfile::tempdir().unwrap();
    let real = tempfile::tempdir().unwrap();
    real_arecord(real.path());
    let path = format!(
        "{}:{}:/usr/bin:/bin",
        dir.path().display(),
        real.path().display()
    );
    let out = arecord(dir.path(), &path, &["-f", "cd", "take.wav"], &[]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("real arecord -f cd take.wav handed=1"),
        "{said}"
    );
    // Handed on already: no further, so two qoop links never exec each other forever.
    let out = arecord(
        dir.path(),
        &path,
        &["take.wav"],
        &[("QOOP_ARECORD_HANDED", "1")],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());
}

/// Claude Code's own probe outside a qoop tab, with no real `arecord`: a failure, so it
/// moves on to its own "no microphone" message.
#[test]
fn claude_codes_probe_with_nothing_to_lend_fails() {
    let dir = tempfile::tempdir().unwrap();
    let probe = [
        "-f",
        "S16_LE",
        "-r",
        "16000",
        "-c",
        "1",
        "-t",
        "raw",
        "/dev/null",
    ];
    let out = arecord(dir.path(), "/usr/bin:/bin", &probe, &[]);
    assert!(!out.status.success());
    // In a tab whose daemon is not running: refused as well, at once.
    let runtime = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let out = arecord(
        dir.path(),
        "/usr/bin:/bin",
        &probe,
        &[
            ("QOOP_TERMINAL", "1"),
            ("QOOP_TOKEN", "t"),
            ("QOOP_RUNTIME_DIR", runtime.path().to_str().unwrap()),
        ],
    );
    assert!(!out.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
